#![allow(unreachable_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
use std::io::Write;

use crossterm::terminal::disable_raw_mode;
use env_logger::{Builder, Env};

use dataman::{
    controller::controller_impl::Controller, error::AppError, model::database::Database, tui::TUI,
    Cli,
};

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    setup_panic_hook();
    setup_logging();
    setup_application()?.run()
}

fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|_| {
        match disable_raw_mode() {
            Ok(()) => println!("disabled raw mode"),
            Err(err) => println!("could not disable raw mode due to {err}"),
        };
    }));
}

fn setup_application() -> Result<Controller, AppError> {
    let time_start = std::time::Instant::now();
    let cli = <Cli as clap::Parser>::parse();
    let paths = cli.paths;
    let database = Database::try_from(paths)?;
    let time_end = std::time::Instant::now();
    log::debug!(
        "Time taken to setup application: {:?}",
        time_end - time_start
    );
    let tui = TUI::new();
    Ok(Controller::new(tui, database))
}

fn setup_logging() {
    let env = Env::default();

    Builder::from_env(env)
        .write_style(env_logger::WriteStyle::Always)
        .format(|buf, record| {
            let timestamp = buf.timestamp_millis();
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
}
