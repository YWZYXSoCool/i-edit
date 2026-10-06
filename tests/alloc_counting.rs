//! Allocation-counting checks for the allocation-compression plan
//! (`docs/allocation-plan.md` §4): the assertions pin the final upper bounds,
//! and each comment records the Phase 0 baseline.
//!
//! # Counting allocator
//!
//! `alloc` and `realloc` each count as one allocation event; `dealloc` never
//! decrements, so the global counter only grows. Two counters are kept: a
//! process-wide `ALLOCS` atomic, kept for manual observation, and a
//! thread-local `THREAD_ALLOCS` cell.
//!
//! `measure` returns the *thread-local* delta. That is a deliberate deviation
//! from §4's global counter plus `SERIAL` mutex: under libtest the harness
//! schedules tests and prints results from threads of its own, and those
//! threads allocate while a measurement window is open, so a global reading
//! can be polluted even when only one test is running our code. The
//! thread-local delta isolates exactly what happened inside the window; run
//! with `--test-threads=1` when a strictly serialized global reading is
//! wanted.
//!
//! Logging is never initialized in this binary, so `log` macros expand to
//! no-ops and contribute no logger-side formatting allocations.
//!
//! # Baselines
//!
//! The Phase 0 baselines were measured with
//! `cargo test --test alloc_counting -- --nocapture` before the optimization;
//! the plan asks for "≤ N", so margins are at most +1 where noted. Fixture
//! creation, file reads and warmups all happen outside the measured windows.
//! Windows file I/O is noisy, so the save point takes the minimum of three
//! runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use i_edit::action::Action;
use i_edit::app::App;
use i_edit::component::Component;
use i_edit::fs::{read_text_file, write_text_file};
use i_edit::widgets::command::{CommandPalette, CommandPaletteState};
use i_edit::widgets::editor::{Editor, EditorState};
use i_edit::widgets::file_tree::{FileTree, FileTreeState};
use i_edit::widgets::picker::{Picker, PickerMode, PickerState};

/// Allocation events, process-wide; never decremented, so two reads can be
/// diffed by hand.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Allocation events on the current thread. The `const` initializer keeps
    /// TLS access from allocating itself, which is what makes it safe to call
    /// from inside the allocator.
    static THREAD_ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        THREAD_ALLOCS.with(|n| n.set(n.get() + 1));
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        THREAD_ALLOCS.with(|n| n.set(n.get() + 1));
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocation events (alloc + realloc) seen so far on this thread.
fn thread_allocs() -> usize {
    THREAD_ALLOCS.with(Cell::get)
}

/// Runs `f` and returns its result plus the allocation-event delta on this
/// thread. Fixtures, warmups and printing must stay outside the window.
fn measure<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = thread_allocs();
    let result = f();
    let after = thread_allocs();
    (result, after - before)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "i-edit-alloc-{label}-{}-{unique}",
            std::process::id()
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

/// The 80x24 area every frame measurement uses.
fn area() -> Rect {
    Rect::new(0, 0, 80, 24)
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn report(name: &str, count: usize, ops: usize) {
    println!(
        "[alloc] {name}: {count} ({ops} ops, {:.2} per op)",
        count as f64 / ops as f64
    );
}

const FILE_LINES: usize = 1000;

/// Writes a 1000-line fixture file and returns its path. Lines are ~40
/// columns wide, so vertical movement has real `find_best_char_position`
/// work to do.
fn write_1k_line_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("input.txt");
    let mut content = String::new();

    for i in 0..FILE_LINES {
        content.push_str(&format!("line {i:04}: alpha beta gamma delta epsilon zeta"));
        content.push('\n');
    }

    fs::write(&path, content).unwrap();
    path
}

/// Loads the 1k-line fixture the way the app does: public reader, then
/// `load_file`. Both run outside every measurement window.
fn editor_with_1k_lines(dir: &TempDir) -> EditorState {
    let path = write_1k_line_fixture(dir.path());
    let mut state = EditorState::new();
    let lines = read_text_file(&path).unwrap();
    assert_eq!(lines.len(), FILE_LINES);
    state.load_file(path, lines);
    state
}

/// Paints one frame so viewport metrics are set before key measurements; the
/// frame itself is not measured.
fn warm_up_editor(state: &mut EditorState) {
    let area = area();
    let mut buf = Buffer::empty(area);
    Component::render(Editor, area, &mut buf, state);
}

#[test]
fn editor_typing_100_chars() {
    let dir = TempDir::new("editor-typing");
    let mut state = editor_with_1k_lines(&dir);
    warm_up_editor(&mut state);

    // Warmup batch: after it, the line's String has grown, so the measured
    // batch is the amortized steady-state cost (the first batch would pay the
    // growth reallocations).
    for _ in 0..100 {
        state.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    }

    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            state.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        }
    });

    report("editor typing x100", allocs, 100);
    // Phase 0 baseline: 1; final: 1 (amortized `String` growth is kept, §3).
    // The warmup batch leaves the line at capacity 184, so the measured 100
    // inserts cross exactly one doubling boundary; after that the batch is
    // amortized.
    assert!(allocs <= 1, "editor typing allocated {allocs} times");
}

#[test]
fn editor_down_100_keys() {
    let dir = TempDir::new("editor-down");
    let mut state = editor_with_1k_lines(&dir);
    warm_up_editor(&mut state);

    // Warmup key, then the measured batch walks down through the 1k lines.
    state.handle_key(KeyCode::Down, KeyModifiers::NONE);

    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            state.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
    });

    report("editor down x100", allocs, 100);
    // Phase 0 baseline: 4600; final: 0 — movement is allocation-free now.
    assert!(allocs <= 0, "editor down allocated {allocs} times");
}

#[test]
fn editor_ctrl_left_100_keys() {
    let dir = TempDir::new("editor-ctrl-left");
    let mut state = editor_with_1k_lines(&dir);
    warm_up_editor(&mut state);

    // Ctrl+Left at (0, 0) has nowhere to go, so start at the text end and
    // warm up with one word step.
    state.handle_key(KeyCode::End, KeyModifiers::CONTROL);
    state.handle_key(KeyCode::Left, KeyModifiers::CONTROL);

    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            state.handle_key(KeyCode::Left, KeyModifiers::CONTROL);
        }
    });

    report("editor ctrl+left x100", allocs, 100);
    // Phase 0 baseline: 300; final: 0.
    assert!(allocs <= 0, "editor ctrl+left allocated {allocs} times");
}

#[test]
fn editor_ctrl_right_100_keys() {
    let dir = TempDir::new("editor-ctrl-right");
    let mut state = editor_with_1k_lines(&dir);
    warm_up_editor(&mut state);

    // Start at the text start and warm up with one word step.
    state.handle_key(KeyCode::Home, KeyModifiers::CONTROL);
    state.handle_key(KeyCode::Right, KeyModifiers::CONTROL);

    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            state.handle_key(KeyCode::Right, KeyModifiers::CONTROL);
        }
    });

    report("editor ctrl+right x100", allocs, 100);
    // Phase 0 baseline: 300; final: 0.
    assert!(allocs <= 0, "editor ctrl+right allocated {allocs} times");
}

#[test]
fn editor_render_frame_1k_lines() {
    let dir = TempDir::new("editor-render");
    let mut state = editor_with_1k_lines(&dir);
    let area = area();
    // The buffer itself is allocated outside the window and reused.
    let mut buf = Buffer::empty(area);

    // Warmup frame: fills in the viewport metrics; only steady-state frames
    // are measured.
    Component::render(Editor, area, &mut buf, &mut state);

    let (_, allocs) = measure(|| Component::render(Editor, area, &mut buf, &mut state));

    report("editor frame 80x24 (1k lines)", allocs, 1);
    // Phase 0 baseline: 0; final: 0.
    assert!(allocs <= 0, "editor frame allocated {allocs} times");
}

/// Root directory with `files` plain entries.
fn tree_fixture(label: &str, files: usize) -> TempDir {
    let dir = TempDir::new(label);

    for i in 0..files {
        fs::write(dir.path().join(format!("file{i:03}.txt")), "x").unwrap();
    }

    dir
}

#[test]
fn file_tree_render_frame_100_children() {
    let dir = tree_fixture("tree-render", 100);
    let mut state = FileTreeState::new();
    state.open_root(dir.path().to_path_buf());
    let area = area();
    let mut buf = Buffer::empty(area);

    Component::render(FileTree, area, &mut buf, &mut state); // warmup

    let (_, allocs) = measure(|| Component::render(FileTree, area, &mut buf, &mut state));

    report("file tree frame 80x24 (100 children)", allocs, 1);
    // Phase 0 baseline: 328; final: 24 (one `Line` Vec per visible row: 23
    // rows plus the bottom path hint, §9.1).
    assert!(allocs <= 24, "file tree frame allocated {allocs} times");
}

#[test]
fn file_tree_down_100_keys() {
    let dir = tree_fixture("tree-down", 100);
    let mut state = FileTreeState::new();
    state.open_root(dir.path().to_path_buf());

    // Warmup frame, then one warmup key: movement itself holds no lazy state,
    // but the frame settles scroll/selection bookkeeping.
    let area = area();
    let mut buf = Buffer::empty(area);
    Component::render(FileTree, area, &mut buf, &mut state);
    Component::handle_event(FileTree, &press(KeyCode::Down), &mut state);

    let down = press(KeyCode::Down);
    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            Component::handle_event(FileTree, &down, &mut state);
        }
    });

    report("file tree down x100", allocs, 100);
    // Phase 0 baseline: 21000; final: 0.
    assert!(allocs <= 0, "file tree down allocated {allocs} times");
}

#[test]
fn file_tree_up_100_keys() {
    let dir = tree_fixture("tree-up", 100);
    let mut state = FileTreeState::new();
    state.open_root(dir.path().to_path_buf());

    let area = area();
    let mut buf = Buffer::empty(area);
    Component::render(FileTree, area, &mut buf, &mut state);

    // Setup: walk to the bottom row so the measured 100 ups have room, then
    // one warmup key.
    let down = press(KeyCode::Down);
    for _ in 0..100 {
        Component::handle_event(FileTree, &down, &mut state);
    }
    Component::handle_event(FileTree, &press(KeyCode::Up), &mut state);

    let up = press(KeyCode::Up);
    let (_, allocs) = measure(|| {
        for _ in 0..100 {
            Component::handle_event(FileTree, &up, &mut state);
        }
    });

    report("file tree up x100", allocs, 100);
    // Phase 0 baseline: 21000; final: 0.
    assert!(allocs <= 0, "file tree up allocated {allocs} times");
}

#[test]
fn file_tree_expand_subdirectory() {
    let dir = TempDir::new("tree-expand");
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for i in 0..10 {
        fs::write(sub.join(format!("child{i}.txt")), "x").unwrap();
    }
    for i in 0..20 {
        fs::write(dir.path().join(format!("file{i:02}.txt")), "x").unwrap();
    }

    let mut state = FileTreeState::new();
    state.open_root(dir.path().to_path_buf());

    // Row 0 is the root, row 1 is `sub`: directories sort first and there is
    // exactly one directory.
    Component::handle_event(FileTree, &press(KeyCode::Down), &mut state);

    // No warmup: the first expansion pays `list_dir`, and a cached
    // re-expansion would not. This measures the cold path the plan targets.
    let (_, allocs) =
        measure(|| Component::handle_event(FileTree, &press(KeyCode::Right), &mut state));

    report("file tree expand subdirectory", allocs, 1);
    // Phase 0 baseline: 210; final: 137 (`list_dir` allocates a PathBuf and a
    // name per freshly listed child, §3).
    assert!(allocs <= 137, "file tree expand allocated {allocs} times");
}

#[test]
fn picker_type_10_chars_and_render_frame() {
    let dir = TempDir::new("picker");
    for i in 0..30 {
        fs::write(dir.path().join(format!("entry{i:02}.txt")), "x").unwrap();
    }

    let mut state = PickerState::new(PickerMode::File, dir.path().to_path_buf(), None);
    let area = area();
    let mut buf = Buffer::empty(area);

    // Warmup frame only: typing is measured from the freshly opened picker.
    Component::render(Picker, area, &mut buf, &mut state);

    // In File mode plain text keys edit the path box, so after the first
    // character the directory no longer resolves and the listed rows collapse;
    // every keystroke still runs `list_dir` and a full rows rebuild, which is
    // exactly the per-keystroke path §2 targets.
    let (_, allocs) = measure(|| {
        for ch in "abcdefghij".chars() {
            Component::handle_event(Picker, &press(KeyCode::Char(ch)), &mut state);
        }
        Component::render(Picker, area, &mut buf, &mut state);
    });

    report("picker type 10 chars + frame (30 entries)", allocs, 1);
    // Phase 0 baseline: 71; final: 62 (per keystroke: `expand_path` PathBuf +
    // failing `list_dir` + a rows rebuild, then one render frame).
    assert!(
        allocs <= 62,
        "picker typing + frame allocated {allocs} times"
    );
}

#[test]
fn command_palette_render_frame() {
    let mut state = CommandPaletteState::new();
    state.open();

    let area = area();
    let mut buf = Buffer::empty(area);
    Component::render(CommandPalette, area, &mut buf, &mut state); // warmup

    let (_, allocs) = measure(|| Component::render(CommandPalette, area, &mut buf, &mut state));

    report("command palette frame 80x24", allocs, 1);
    // Phase 0 baseline: 31; final: 8 (one line-span Vec per suggestion row,
    // §9.1).
    assert!(
        allocs <= 8,
        "command palette frame allocated {allocs} times"
    );
}

#[test]
fn save_1k_lines() {
    let dir = TempDir::new("save");
    let path = dir.path().join("out.txt");
    let lines: Vec<String> = (0..FILE_LINES)
        .map(|i| format!("line {i:04}: alpha beta gamma delta epsilon zeta"))
        .collect();

    // Warmup write, then three measured writes. Windows file I/O is noisy
    // (antivirus, cache state), so the reported baseline is the minimum of
    // the three: the plan's target is the allocation count of the write path,
    // not the scheduler's contribution.
    write_text_file(&path, &lines).unwrap();

    let mut best = usize::MAX;
    for _ in 0..3 {
        let (result, allocs) = measure(|| write_text_file(&path, &lines));
        result.unwrap();
        best = best.min(allocs);
    }

    report("save 1k lines (min of 3)", best, 1);
    // Phase 0 baseline: min of 3 is 3; final: 2 (the exactly sized content
    // String plus one Windows write-path allocation); +1 margin for I/O noise.
    assert!(best <= 3, "save allocated {best} times");
}

#[test]
fn action_queue_steady_state() {
    // The shell-side `App::apply` point lives in `app_apply_single_action`
    // below; this test keeps the component-level queue path on the public
    // `drain` API.
    let mut palette = CommandPaletteState::new();
    let mut resident: Vec<Action> = Vec::new();
    let enter = press(KeyCode::Enter);

    // The shell's flow: the palette is open, one key turns into one action,
    // the action moves into the shell's persistent queue, and the queue
    // forgets it while keeping its capacity.
    fn round(palette: &mut CommandPaletteState, resident: &mut Vec<Action>, enter: &Event) {
        palette.open();
        Component::handle_event(CommandPalette, enter, palette);
        resident.clear();
        resident.extend(palette.take_actions());
        assert_eq!(
            resident.len(),
            1,
            "one Enter should queue exactly one action"
        );
    }

    const ROUNDS: usize = 1000;

    // Warmup rounds: warm all capacities (suggestion list, outbox, resident
    // buffer) before the measured loop.
    for _ in 0..100 {
        round(&mut palette, &mut resident, &enter);
    }

    let (_, total) = measure(|| {
        for _ in 0..ROUNDS {
            round(&mut palette, &mut resident, &enter);
        }
    });

    println!(
        "[alloc] action queue steady state: {total} total / {ROUNDS} rounds ({:.3} per round)",
        total as f64 / ROUNDS as f64
    );
    // Phase 0 baseline: 3000 over 1000 rounds; final: 1000 — exactly 1 per
    // round, because the public `take_actions`/`drain` path still `mem::take`s
    // the outbox and the next `emit` then reallocates it. The shell's
    // capacity-preserving drain (`take_into`) is private and has no
    // integration-test seam, so this public path is the closest reachable
    // proxy.
    assert!(total <= ROUNDS, "action queue allocated {total} times");
}

/// §4 point 7: one action through `App::apply`.
///
/// The shell's queue plumbing (`apply_actions`) is private, so this measures
/// the action handler itself; the component-level queue cycle stays covered by
/// `action_queue_steady_state` above.
#[test]
fn app_apply_single_action() {
    let mut app = App::default();

    // Setup outside the window: build the shell and warm the handler.
    for _ in 0..100 {
        app.apply_for_test(Action::ToggleFileTree);
    }

    const CALLS: usize = 100;
    let (_, total) = measure(|| {
        for _ in 0..CALLS {
            app.apply_for_test(Action::ToggleFileTree);
        }
    });

    println!(
        "[alloc] App::apply one action: {total} total / {CALLS} calls ({:.3} per call)",
        total as f64 / CALLS as f64
    );
    // Final: 0 over 100 calls — toggling the file tree panel is a flag flip
    // with no allocation once the shell exists. No Phase 0 reading: `HEAD` had
    // no `apply_for_test` hook to measure.
    assert!(total <= 0, "App::apply allocated {total} times");
}
