use std::env;

use i_edit::Result;
use i_edit::app::App;

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

    ratatui::run(|terminal| App::default().run(terminal))
}
