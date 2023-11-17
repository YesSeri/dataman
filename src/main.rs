use dataman::{controller::Controller, model::database::Database, tui::TUI};
use std::{error::Error, path::Path};

mod cli;
use cli::Cli;

fn main() -> Result<(), Box<dyn Error>> {
    let cli = <Cli as clap::Parser>::parse();
    let database = Database::try_from(cli.path).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);

    if let Err(err) = controller.run() {
        eprintln!("Program has quit due to error: {err}")
    }
    Ok(())
}
