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
use rusqlite::Connection;

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    TUI::install_panic_hook();
    setup_logging();
    let cli = <Cli as clap::Parser>::parse();
    Controller::run_paths(cli.paths, TUI::default())
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
