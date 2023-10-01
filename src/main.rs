use std::{error::Error, path::Path};

use dataman::{controller::Controller, libstuff::db::Database, tui::TUI};

fn main() -> Result<(), Box<dyn Error>> {
    let p = Path::new("assets/data.csv");

    let database = Database::try_from(p).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);

    let result = controller.run();

    if let Err(err) = result {
        eprintln!("Program has quit due to error: {err}")
    }

    Ok(())
}
