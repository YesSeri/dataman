#![allow(unreachable_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
use dataman::{controller::Controller, error::AppError, model::database::Database, tui::TUI, Cli};
use log::{error, info};

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    if cfg!(debug_assertions) {
        std::env::set_var("RUST_LOG", "debug");
    } else {
        std::env::set_var("RUST_LOG", "error");
    }
    env_logger::init();
    let cli = <Cli as clap::Parser>::parse();
    let database = Database::try_from(cli.path).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);

    if let Err(err) = controller.run() {
        eprintln!("Program has quit due to error: {err}")
    }
    Ok(())
}
