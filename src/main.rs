use std::env;
use std::io::stdout;

use crossterm::event::EnableBracketedPaste;
use crossterm::execute;
use i_edit::Result;
use i_edit::app::App;
use i_edit::storage::Storage;

use tui_logger::TuiLoggerFile;

fn main() -> Result<()> {
    // Quiet by default: the trace of every queued action is only wanted while
    // chasing a problem, so it takes an explicit `I_EDIT_LOG=trace`.
    let level = env::var("I_EDIT_LOG")
        .ok()
        .and_then(|level| level.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);

    tui_logger::init_logger(level).unwrap();
    tui_logger::set_default_level(level);
    tui_logger::set_log_file(TuiLoggerFile::new("i-edit.log"));

    color_eyre::install()?;
    crossterm::terminal::enable_raw_mode()?;
    execute!(stdout(), EnableBracketedPaste)?;

    // The session is loaded before the first frame and written after the last,
    // on the way out as well as on the way in: a crash of the TUI still leaves
    // the next run knowing what was open.
    let mut app = App::new(Storage::load());
    let ran = ratatui::run(|terminal| app.run(terminal));
    let persisted = app.persist();

    // The run's own error wins: it is what the user was doing. A failed
    // persist only surfaces when nothing else went wrong.
    ran.and(persisted)
}
