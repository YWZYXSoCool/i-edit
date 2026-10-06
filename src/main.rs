use i_edit::Result;
use i_edit::app::App;

use tui_logger::TuiLoggerFile;

fn main() -> Result<()> {
    tui_logger::init_logger(log::LevelFilter::Trace).unwrap();
    tui_logger::set_default_level(log::LevelFilter::Trace);
    tui_logger::set_log_file(TuiLoggerFile::new("i-edit.log"));

    color_eyre::install()?;

    ratatui::run(|terminal| App::default().run(terminal))
}
