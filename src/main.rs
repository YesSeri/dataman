use std::{error::Error, path::Path};

use dataman::{controller::Controller, error::log, model::database::Database, tui::TUI};

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 2 {
        eprintln!("Usage: dataman <path>");
        std::process::exit(1);
    }
    let path_arg = args.get(1).expect("No path provided");
    let path = Path::new(path_arg);
    log(format!("path: {:?}", path));

    let database = Database::try_from(path).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);

    if let Err(err) = controller.run() {
        eprintln!("Program has quit due to error: {err}")
    }
    Ok(())
}
