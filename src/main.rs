#![allow(unreachable_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]

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

fn main() {
    std::panic::set_hook(Box::new(|_| {
        match disable_raw_mode() {
            Ok(_) => println!("disabled raw mode"),
            Err(err) => println!("could not disable raw mode due to {err}"),
        };
    }));
    // if not release mode, print logs
    // if release mode, logs are not printed
    setup_logging();
    let mut controller = setup_application().unwrap_or_else(|err| {
        eprintln!("Could not start due to {err}");
        exit(1);
        // '1' indicates an error setting up the application,
        // dunno if this is a good way to do it,
        // should probably be more fine-grained.
    });

    if let Err(err) = controller.run() {
        eprintln!("Program has quit due to error: {err}");
        exit(2);
        // '2' indicates an error running the application
    }
}

fn setup_application() -> Result<Controller, AppError> {
    let cli = <Cli as clap::Parser>::parse();
    let paths = cli.paths;
    let database = Database::try_from(paths)?;
    let tui = TUI::new();
    Ok(Controller::new(tui, database))
}

fn setup_logging() {
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
}
