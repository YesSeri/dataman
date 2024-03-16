#![allow(unreachable_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
use dataman::{controller::Controller, error::AppError, model::database::Database, tui::TUI, Cli};
use log::debug;

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    setup_logging();
    let cli = <Cli as clap::Parser>::parse();
    let database = Database::try_from(cli.path).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);

    if let Err(err) = controller.run() {
        eprintln!("Program has quit due to error: {err}")
    }
    Ok(())
}

fn setup_logging(){
    // if RUST_LOG is not set, set it to debug if in debug mode
    let env_rust_log = std::env::var("RUST_LOG");
    if env_rust_log.is_err() {
        if cfg!(debug_assertions) {
            std::env::set_var("RUST_LOG", "debug");
        } else {
            std::env::set_var("RUST_LOG", "error");
        }
    }
    env_logger::init();
    debug!("Running in debug mode");
}
