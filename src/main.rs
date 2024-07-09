use std::io::Write;
use std::path::PathBuf;
use std::process::exit;

use crossterm::terminal::disable_raw_mode;
use env_logger::{Builder, Env};
use log::error;

use dataman::{
    controller::controller_impl::Controller, error::AppError, model::database::Database, tui::TUI,
    Cli,
};

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    setup_panic_hook()?;
    setup_logging()?;
    setup_application()?.run()
}

fn setup_panic_hook() -> Result<(), AppError> {
    std::panic::set_hook(Box::new(|_| {
        match disable_raw_mode() {
            Ok(_) => println!("disabled raw mode"),
            Err(err) => println!("could not disable raw mode due to {err}"),
        };
    }));
    Ok(())
}

fn setup_application() -> Result<Controller, AppError> {
    let cli = <Cli as clap::Parser>::parse();
    let paths = cli.paths;
    let database = Database::try_from(paths)?;
    let tui = TUI::new();
    Ok(Controller::new(tui, database))
}

fn setup_logging() -> Result<(), AppError> {
    let env = Env::default();

    Builder::from_env(env)
        .write_style(env_logger::WriteStyle::Always)
        .format(|buf, record| {
            let timestamp = buf.timestamp();
            let write_style = buf.default_level_style(record.level());

            writeln!(
                buf,
                "[{} {write_style}{: >5}{write_style:#} {: >3}:{:<22}]: {}",
                timestamp,
                record.level(),
                record.line().unwrap_or(0),
                record.file().unwrap_or("file/???"),
                record.args(),
            )
        })
        .init();
    Ok(())
}
