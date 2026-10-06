//! Right-hand file tree: root path, lazy-loading entries, selection.
//!
//! The visible list is flattened from the expanded directories in depth-first
//! order. A directory's children are read with [`list_dir`] the first time it
//! is expanded and kept in a per-directory cache afterwards, so opening a
//! folder never walks the whole tree up front — that laziness is also what
//! keeps symlink cycles harmless.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::action::{Action, Actions};
use crate::component::Component;
use crate::fs::{DirEntry, list_dir};
use crate::icon;

use crossterm::event::{Event, KeyCode};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

/// Placeholder shown until a folder is opened.
const NO_FOLDER_HINT: &str = "no folder opened · Ctrl+Shift+O";

/// Shortest panel that can afford the root-path hint without crowding out the
/// entries: one row for the hint, at least two for the tree.
const ROOT_HINT_MIN_HEIGHT: u16 = 3;

/// The border, hidden entries and the root-path hint share this dim tone.
const DIM_STYLE: Style = Style::new().fg(Color::DarkGray);

/// The selected row is inverted: black on white reads as a highlight bar on
/// every terminal background.
const SELECTED_STYLE: Style = Style::new().fg(Color::Black).bg(Color::White);

/// One line of the flattened tree.
#[derive(Debug)]
pub(crate) struct Row {
    path: PathBuf,
    name: String,
    is_dir: bool,
    is_hidden: bool,
    depth: usize,
    expanded: bool,
}

/// Cache for one directory in the tree.
#[derive(Debug)]
struct Dir {
    /// Children loaded the first time the directory is expanded; `None` means
    /// it was never read. A failed read is cached as an empty list: a
    /// directory that cannot be read should show nothing, not be retried on
    /// every expansion.
    children: Option<Vec<DirEntry>>,
    /// Whether the children appear in the flattened rows.
    expanded: bool,
}

#[derive(Debug, Default)]
pub struct FileTreeState {
    root: Option<PathBuf>,
    /// Per-directory cache, keyed by path: one `PathBuf` per directory rather
    /// than one in each of several maps.
    dirs: HashMap<PathBuf, Dir>,
    /// Flattened rows, rebuilt on structural changes and read as-is otherwise.
    rows: Vec<Row>,
    /// Index into the flattened rows.
    selected: usize,
    /// First row of the flattened list shown; fixed up at render time, when
    /// the available height is known.
    scroll: usize,
    actions: Actions,
}

impl FileTreeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the tree root, expands it, and loads its children.
    ///
    /// Everything from a previous root is dropped: its cache, expansion set,
    /// selection and scroll all belong to the old tree.
    pub fn open_root(&mut self, root: PathBuf) {
        self.root = Some(root.clone());
        self.dirs.clear();
        self.selected = 0;
        self.scroll = 0;
        self.set_expanded(&root, true);
    }

    /// The root currently shown, if any.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Takes everything this component has asked for since the last drain.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    /// The flattened rows as of the last structural change.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Moves the selection by `delta` rows, clamped to the ends.
    fn move_selection(&mut self, delta: isize) {
        let row_count = self.rows.len();
        if row_count == 0 {
            return;
        }

        let last = row_count as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// `Right`: expands a collapsed directory, steps into an expanded one, and
    /// does nothing on a file.
    fn expand_or_step_in(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if !row.is_dir {
            return;
        }

        let path = row.path.clone();
        let expanded = row.expanded;
        let depth = row.depth;

        if !expanded {
            self.set_expanded(&path, true);
            self.clamp_selection();
            return;
        }

        // Already open: step into the first child, which sits directly below
        // its parent in depth-first order. A childless directory stays put.
        if let Some(child) = self.rows.get(self.selected + 1)
            && child.depth == depth + 1
        {
            self.selected += 1;
        }
    }

    /// `Left`: collapses an expanded directory, otherwise jumps to the parent
    /// row. The root has no parent, so that case is a no-op.
    fn collapse_or_step_out(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };

        let path = row.path.clone();
        let expanded_dir = row.is_dir && row.expanded;
        let depth = row.depth;

        if expanded_dir {
            self.set_expanded(&path, false);
            self.clamp_selection();
            return;
        }

        let Some(parent_depth) = depth.checked_sub(1) else {
            return;
        };

        // Depth-first order puts the nearest ancestor directly above `row`:
        // the first row at one level less.
        if let Some(index) = self.rows[..self.selected]
            .iter()
            .rposition(|r| r.depth == parent_depth)
        {
            self.selected = index;
        }
    }

    /// `Enter`: a directory toggles, a file asks the shell to load it.
    fn activate(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let expanded = row.expanded;

        if is_dir {
            self.set_expanded(&path, !expanded);
        } else {
            self.actions.emit(Action::LoadFile(path));
        }

        self.clamp_selection();
    }

    /// Keeps the selection pointing at a row that still exists.
    fn clamp_selection(&mut self) {
        let row_count = self.rows.len();
        if row_count == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else {
            self.selected = self.selected.min(row_count - 1);
        }
    }

    /// Expands or collapses `path`, reading its children on their first
    /// expansion.
    ///
    /// A directory whose read fails caches an empty list, so the failure is
    /// not retried on every expansion. Children read earlier stay cached
    /// through a collapse. Collapsing a directory with no cache entry does
    /// nothing; the rows are rebuilt either way, because the caller may be
    /// reacting to a tree that changed elsewhere.
    fn set_expanded(&mut self, path: &Path, expanded: bool) {
        if expanded {
            let dir = self.dirs.entry(path.to_path_buf()).or_insert_with(|| Dir {
                children: None,
                expanded: false,
            });
            if dir.children.is_none() {
                dir.children = Some(list_dir(path).unwrap_or_default());
            }
            dir.expanded = true;
        } else if let Some(dir) = self.dirs.get_mut(path) {
            dir.expanded = false;
        }

        self.rebuild_rows();
    }

    /// Rebuilds the flattened rows from the expanded directories.
    ///
    /// Depth-first pre-order, using an explicit stack so the walk is bounded
    /// by the user's expansions rather than by the file system: nothing is
    /// recursed into until its directory was expanded and read. The vector is
    /// cleared in place, so its capacity is reused across rebuilds.
    fn rebuild_rows(&mut self) {
        self.rows.clear();

        let Some(root) = self.root.as_deref() else {
            return;
        };

        let mut stack = vec![Row {
            path: root.to_path_buf(),
            name: root_name(root),
            is_dir: true,
            is_hidden: false,
            depth: 0,
            expanded: false,
        }];

        while let Some(mut row) = stack.pop() {
            row.expanded = row.is_dir && self.dirs.get(&row.path).is_some_and(|dir| dir.expanded);

            if row.expanded
                && let Some(children) = self
                    .dirs
                    .get(&row.path)
                    .and_then(|dir| dir.children.as_ref())
            {
                let child_depth = row.depth + 1;
                // Reverse so the first child comes off the stack first.
                stack.extend(children.iter().rev().map(|child| Row {
                    path: child.path.clone(),
                    name: child.name.clone(),
                    is_dir: child.is_dir,
                    is_hidden: child.is_hidden,
                    depth: child_depth,
                    expanded: false,
                }));
            }

            self.rows.push(row);
        }
    }

    /// Scrolls the minimum amount that keeps the selection visible.
    ///
    /// Called at render time because only then is the rows height known; the
    /// key handlers leave the scroll alone and let this fix it up.
    fn keep_selection_visible(&mut self, row_count: usize, rows_height: usize) {
        if rows_height == 0 || row_count == 0 {
            self.scroll = 0;
            return;
        }

        // A taller panel can leave the old scroll past the end of a shorter
        // list.
        self.scroll = self.scroll.min(row_count.saturating_sub(rows_height));

        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows_height {
            self.scroll = self.selected + 1 - rows_height;
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FileTree;

impl Component for FileTree {
    type State = FileTreeState;

    /// Only the keys the spec binds are handled here; `Esc` belongs to the
    /// shell, which decides between leaving the tree and quitting.
    fn handle_event(self, event: &Event, state: &mut FileTreeState) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        match key.code {
            KeyCode::Up => state.move_selection(-1),
            KeyCode::Down => state.move_selection(1),
            KeyCode::Right => state.expand_or_step_in(),
            KeyCode::Left => state.collapse_or_step_out(),
            KeyCode::Enter => state.activate(),
            _ => {}
        }
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut FileTreeState) {
        if area.is_empty() {
            return;
        }

        if state.root.is_none() {
            // Nothing is open: a one-line hint is the whole panel.
            Line::from(Span::styled(NO_FOLDER_HINT, DIM_STYLE)).render(area, buf);
            return;
        }

        // Left border only: a separator between the editor and the tree, not
        // an enclosure, so the rows start at the very next column.
        Block::default()
            .borders(Borders::LEFT)
            .border_style(DIM_STYLE)
            .render(area, buf);

        // Manual inner rect: `Rect::inner` removes the margin from both
        // sides, but the border only occupies the left column.
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y,
            width: area.width.saturating_sub(1),
            height: area.height,
        };
        if inner.is_empty() {
            return;
        }

        // The hint hugs the bottom edge, but the entries keep at least two
        // rows: a shorter panel drops the hint instead.
        let hint_height = u16::from(area.height >= ROOT_HINT_MIN_HEIGHT);
        let rows_height = inner.height.saturating_sub(hint_height) as usize;

        let row_count = state.rows().len();
        state.keep_selection_visible(row_count, rows_height);

        for (offset, row) in state
            .rows()
            .iter()
            .skip(state.scroll)
            .take(rows_height)
            .enumerate()
        {
            let row_area = Rect {
                x: inner.x,
                y: inner.y + offset as u16,
                width: inner.width,
                height: 1,
            };
            render_row(row, row_area, state.selected == state.scroll + offset, buf);
        }

        if hint_height == 1 {
            let hint_area = Rect {
                x: inner.x,
                y: inner.bottom() - 1,
                width: inner.width,
                height: 1,
            };
            if let Some(root) = state.root.as_deref() {
                Line::from(Span::styled(root.to_string_lossy(), DIM_STYLE)).render(hint_area, buf);
            }
        }
    }
}

/// Draws one row: indentation, expand marker, icon and name.
///
/// The selected row is inverted across its whole width, so the highlight reads
/// as a bar rather than just coloured text; selection wins over the dim tone of
/// a hidden entry.
fn render_row(row: &Row, area: Rect, selected: bool, buf: &mut Buffer) {
    let style = if selected {
        SELECTED_STYLE
    } else if row.is_hidden {
        DIM_STYLE
    } else {
        Style::default()
    };

    if selected {
        for x in area.x..area.right() {
            buf[(x, area.y)].set_char(' ').set_style(style);
        }
    }

    let marker = if !row.is_dir {
        " "
    } else if row.expanded {
        icon::CHEVRON_DOWN
    } else {
        icon::CHEVRON
    };
    let folder = if !row.is_dir {
        icon::FILE
    } else if row.expanded {
        icon::FOLDER_OPEN
    } else {
        icon::FOLDER
    };

    // Two indent columns per level, written straight into the buffer; the
    // part that does not fit is skipped just like the old text was clipped.
    let indent = u16::try_from(row.depth.saturating_mul(2))
        .unwrap_or(u16::MAX)
        .min(area.width);
    for x in area.x..area.x.saturating_add(indent) {
        buf[(x, area.y)].set_char(' ').set_style(style);
    }

    let text = Rect {
        x: area.x.saturating_add(indent),
        y: area.y,
        width: area.width.saturating_sub(indent),
        height: 1,
    };
    Line::from(vec![
        Span::styled(marker, style),
        Span::styled(folder, style),
        Span::styled(" ", style),
        Span::styled(row.name.as_str(), style),
    ])
    .render(text, buf);
}

/// Name shown for the root row. A drive or share root has no file name, so it
/// falls back to the full display path instead of an empty label.
fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{FileTree, FileTreeState};
    use crate::action::Action;
    use crate::component::Component;
    use crate::icon;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique directory that deletes itself, so tests can run in parallel and
    /// leave nothing behind even when they panic.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "i-edit-tree-test-{}-{}",
                std::process::id(),
                unique
            ));

            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn keys(state: &mut FileTreeState, codes: &[KeyCode]) {
        for &code in codes {
            Component::handle_event(FileTree, &press(code), state);
        }
    }

    fn row_names(state: &FileTreeState) -> Vec<String> {
        state.rows().iter().map(|row| row.name.clone()).collect()
    }

    /// Root with two directories, a hidden file and a regular file; `src`
    /// holds two files of its own.
    fn fixture() -> TempDir {
        let dir = TempDir::new();
        fs::create_dir(dir.path().join("docs")).unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join(".hidden"), "secret").unwrap();
        fs::write(dir.path().join("readme.md"), "# readme").unwrap();
        fs::write(dir.path().join("src").join("app.rs"), "// app").unwrap();
        fs::write(dir.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        dir
    }

    fn open_fixture() -> (TempDir, FileTreeState) {
        let dir = fixture();
        let mut state = FileTreeState::new();
        state.open_root(dir.path().to_path_buf());
        (dir, state)
    }

    fn root_name(dir: &TempDir) -> String {
        dir.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn a_fresh_state_has_no_root_and_renders_the_placeholder() {
        let mut state = FileTreeState::new();
        assert!(state.root().is_none());

        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        Component::render(FileTree, area, &mut buf, &mut state);

        let row: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row.trim_end(), "no folder opened · Ctrl+Shift+O");
        assert_eq!(buf[(0, 0)].style().fg, Some(Color::DarkGray));
        assert_eq!(buf[(0, 1)].symbol(), " ");
    }

    #[test]
    fn open_root_shows_the_root_and_its_children_dirs_first() {
        let (dir, state) = open_fixture();

        assert_eq!(state.root(), Some(dir.path()));

        let root = root_name(&dir);
        assert_eq!(
            row_names(&state),
            [root.as_str(), "docs", "src", ".hidden", "readme.md"]
        );

        let rows = state.rows();
        assert!(rows[0].is_dir && rows[0].expanded);
        // Nothing below the root has been read yet.
        assert!(!state.dirs.contains_key(&dir.path().join("src")));
    }

    #[test]
    fn right_expands_a_directory_and_loads_its_children() {
        let (dir, mut state) = open_fixture();
        assert!(!state.dirs.contains_key(&dir.path().join("src")));

        keys(&mut state, &[KeyCode::Down, KeyCode::Down]); // root -> docs -> src
        keys(&mut state, &[KeyCode::Right]);

        assert!(state.dirs.contains_key(&dir.path().join("src")));
        assert_eq!(state.selected, 2, "expanding does not move the selection");

        let root = root_name(&dir);
        assert_eq!(
            row_names(&state),
            [
                root.as_str(),
                "docs",
                "src",
                "app.rs",
                "main.rs",
                ".hidden",
                "readme.md"
            ]
        );
    }

    #[test]
    fn right_on_an_expanded_directory_steps_into_the_first_child() {
        let (_dir, mut state) = open_fixture();

        keys(&mut state, &[KeyCode::Down, KeyCode::Down, KeyCode::Right]); // expand src
        keys(&mut state, &[KeyCode::Right]); // step into app.rs
        assert_eq!(state.rows()[state.selected].name, "app.rs");

        keys(&mut state, &[KeyCode::Down, KeyCode::Right]); // main.rs; Right on a file does nothing
        assert_eq!(state.rows()[state.selected].name, "main.rs");
        assert_eq!(state.rows().len(), 7);
    }

    #[test]
    fn left_collapses_an_expanded_directory_then_jumps_to_its_parent() {
        let (dir, mut state) = open_fixture();

        keys(
            &mut state,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Right, KeyCode::Right],
        ); // src expanded, app.rs selected
        keys(&mut state, &[KeyCode::Left]); // back to src
        assert_eq!(state.selected, 2);
        assert!(
            state
                .dirs
                .get(&dir.path().join("src"))
                .is_some_and(|d| d.expanded)
        );

        keys(&mut state, &[KeyCode::Left]); // collapse src
        assert!(
            !state
                .dirs
                .get(&dir.path().join("src"))
                .is_some_and(|d| d.expanded)
        );
        assert_eq!(state.selected, 2);
        assert_eq!(row_names(&state).len(), 5);

        keys(&mut state, &[KeyCode::Left]); // src -> root
        assert_eq!(state.selected, 0);

        keys(&mut state, &[KeyCode::Left]); // root is expanded: collapse it
        assert_eq!(row_names(&state).len(), 1);

        keys(&mut state, &[KeyCode::Left]); // collapsed root has no parent
        assert_eq!(state.selected, 0);
        assert_eq!(row_names(&state).len(), 1);
    }

    #[test]
    fn enter_on_a_file_requests_loading_it() {
        let (dir, mut state) = open_fixture();

        keys(
            &mut state,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Right, KeyCode::Down],
        ); // app.rs
        keys(&mut state, &[KeyCode::Enter]);

        let expected = dir.path().join("src").join("app.rs");
        assert_eq!(state.take_actions(), vec![Action::LoadFile(expected)]);
        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn enter_on_a_directory_toggles_it() {
        let (_dir, mut state) = open_fixture();

        keys(&mut state, &[KeyCode::Down, KeyCode::Down]); // src
        keys(&mut state, &[KeyCode::Enter]);
        assert_eq!(row_names(&state).len(), 7);
        assert_eq!(state.selected, 2);

        keys(&mut state, &[KeyCode::Enter]);
        assert_eq!(row_names(&state).len(), 5);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn hidden_entries_are_listed_and_rendered_dim() {
        let (_dir, mut state) = open_fixture();
        assert!(row_names(&state).contains(&".hidden".to_string()));

        let area = Rect::new(0, 0, 24, 7);
        let mut buf = Buffer::empty(area);
        Component::render(FileTree, area, &mut buf, &mut state);

        // `.hidden` is the fourth row, one level deep: after the border, two
        // indent columns and the marker column, the file icon sits at column 4.
        assert_eq!(buf[(4, 3)].symbol(), icon::FILE);
        assert_eq!(buf[(4, 3)].style().fg, Some(Color::DarkGray));
        // A regular file keeps the terminal's own colours.
        assert_eq!(buf[(4, 4)].symbol(), icon::FILE);
        assert_ne!(buf[(4, 4)].style().fg, Some(Color::DarkGray));

        // Selecting it inverts the whole row, dimness included.
        keys(&mut state, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(state.rows()[state.selected].name, ".hidden");

        let mut buf = Buffer::empty(area);
        Component::render(FileTree, area, &mut buf, &mut state);

        assert_eq!(buf[(4, 3)].style().fg, Some(Color::Black));
        assert_eq!(buf[(4, 3)].style().bg, Some(Color::White));
        assert_eq!(buf[(area.width - 1, 3)].style().bg, Some(Color::White));
        // The separator stays outside the highlight.
        assert_eq!(buf[(0, 3)].style().fg, Some(Color::DarkGray));
        assert_ne!(buf[(0, 3)].style().bg, Some(Color::White));
    }

    #[test]
    fn selection_stays_clamped_after_collapsing() {
        let (_dir, mut state) = open_fixture();

        keys(&mut state, &[KeyCode::Down, KeyCode::Down, KeyCode::Right]); // expand src
        keys(&mut state, &[KeyCode::Down]); // step into app.rs
        assert_eq!(state.rows()[state.selected].name, "app.rs");

        keys(&mut state, &[KeyCode::Left]); // app.rs -> src
        assert_eq!(state.selected, 2);
        keys(&mut state, &[KeyCode::Left]); // collapse src
        assert_eq!(state.selected, 2);
        assert_eq!(state.rows().len(), 5);

        // The ends clamp rather than run past the shorter list.
        keys(
            &mut state,
            &[KeyCode::Up, KeyCode::Up, KeyCode::Up, KeyCode::Up],
        );
        assert_eq!(state.selected, 0);
        keys(&mut state, &[KeyCode::Down; 9]);
        assert_eq!(state.selected, state.rows().len() - 1);
    }

    #[test]
    fn collapsing_an_ancestor_under_the_selection_clamps_on_the_next_key() {
        let (dir, mut state) = open_fixture();

        keys(
            &mut state,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down],
        ); // readme.md, the last row
        assert_eq!(state.selected, 4);

        // Only an external refresh could collapse an ancestor without moving
        // the selection first; the key handlers must still cope with the stale
        // index.
        state.set_expanded(dir.path(), false);
        keys(&mut state, &[KeyCode::Up]);

        assert_eq!(state.selected, 0);
        assert_eq!(row_names(&state).len(), 1);
    }

    #[test]
    fn rendering_into_a_tiny_buffer_does_not_panic() {
        let (_dir, mut state) = open_fixture();

        for (width, height) in [(0, 0), (1, 1), (1, 2), (2, 2), (3, 3), (4, 10), (10, 4)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            Component::render(FileTree, area, &mut buf, &mut state);
        }

        let mut fresh = FileTreeState::new();
        for (width, height) in [(0, 0), (2, 1)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            Component::render(FileTree, area, &mut buf, &mut fresh);
        }
    }

    #[test]
    fn the_root_path_hint_takes_the_bottom_row_when_there_is_room() {
        let (dir, mut state) = open_fixture();

        // Exactly as wide as the path, so the clipped hint can be compared
        // without trailing blanks.
        let displayed = dir.path().display().to_string();
        let width = (displayed.chars().count() as u16 + 1).min(500);
        let area = Rect::new(0, 0, width, 4);
        let mut buf = Buffer::empty(area);
        Component::render(FileTree, area, &mut buf, &mut state);

        let hint: String = (1..area.width)
            .map(|x| buf[(x, area.height - 1)].symbol())
            .collect();
        let expected: String = displayed.chars().take(width as usize - 1).collect();
        assert_eq!(hint, expected);
        assert_eq!(buf[(1, area.height - 1)].style().fg, Some(Color::DarkGray));

        // A two-row panel is too short: both rows stay with the entries.
        let short = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(short);
        Component::render(FileTree, short, &mut buf, &mut state);
        assert_eq!(buf[(3, 1)].symbol(), icon::CHEVRON); // docs, collapsed
    }

    #[test]
    fn open_root_resets_the_previous_tree() {
        let (first, mut state) = open_fixture();
        let first_src = first.path().join("src");

        keys(
            &mut state,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Right, KeyCode::Down],
        ); // expand src, select app.rs
        state.scroll = 2;
        assert_eq!(state.selected, 3);
        assert!(state.dirs.contains_key(&first_src));

        let second = TempDir::new();
        state.open_root(second.path().to_path_buf());

        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll, 0);
        assert!(!state.dirs.contains_key(&first_src));
        assert_eq!(state.dirs.len(), 1);
        assert_eq!(state.dirs.values().filter(|dir| dir.expanded).count(), 1);
        assert!(
            state
                .dirs
                .get(second.path())
                .is_some_and(|dir| dir.expanded)
        );
        assert_eq!(state.rows().len(), 1);
    }

    #[test]
    fn a_root_that_cannot_be_read_still_shows_its_own_row() {
        let dir = TempDir::new();
        let missing = dir.path().join("missing");

        let mut state = FileTreeState::new();
        state.open_root(missing.clone());

        assert_eq!(state.root(), Some(missing.as_path()));
        let rows = state.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_dir && rows[0].expanded);
    }

    #[test]
    fn keys_that_are_not_bound_are_ignored() {
        let (_dir, mut state) = open_fixture();

        keys(
            &mut state,
            &[KeyCode::Esc, KeyCode::Char('x'), KeyCode::PageDown],
        );

        assert_eq!(state.selected, 0);
        assert_eq!(state.rows().len(), 5);
        assert!(state.take_actions().is_empty());
    }
}
