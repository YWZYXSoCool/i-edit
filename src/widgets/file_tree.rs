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
    /// Whether the shell's keyboard focus is on the tree. Only a focused tree
    /// highlights the selected row.
    focused: bool,
    /// First row of the flattened list shown; fixed up at render time, when
    /// the available height is known.
    scroll: usize,
    /// Expansion changes since the last drain. Components cannot touch
    /// storage, so the shell picks these up and persists them.
    expansion_changes: Vec<(PathBuf, bool)>,
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

    /// Sets whether the shell's keyboard focus is on the tree.
    ///
    /// Leaving the tree keeps its selection index, but stops highlighting the
    /// row: the highlight marks where the next key would act, so it goes away
    /// with the keys while the index is remembered for the way back.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Takes everything this component has asked for since the last drain.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    /// Moves everything this component has asked for onto the end of `out`.
    pub fn take_actions_into(&mut self, out: &mut Vec<Action>) {
        self.actions.take_into(out)
    }

    /// Moves the directories expanded or collapsed since the last call onto
    /// `out`, so the shell can remember them across runs.
    pub fn take_expansion_changes_into(&mut self, out: &mut Vec<(PathBuf, bool)>) {
        out.append(&mut self.expansion_changes);
    }

    /// The flattened rows as of the last structural change.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The selected row index, for the shell's tests.
    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the tree renders as focused, for the shell's tests.
    #[cfg(test)]
    pub(crate) fn is_focused(&self) -> bool {
        self.focused
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
    /// Public so the shell can rebuild last run's expansion from storage;
    /// every call is reported through [`Self::take_expansion_changes_into`].
    pub fn set_expanded(&mut self, path: &Path, expanded: bool) {
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

        self.expansion_changes.push((path.to_path_buf(), expanded));
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
            let selected = state.focused && state.selected == state.scroll + offset;
            render_row(row, row_area, selected, buf);
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
    let icon = if !row.is_dir {
        icon::icon_for(&row.name)
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
        Span::styled(" ", style),
        Span::styled(icon, style),
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
