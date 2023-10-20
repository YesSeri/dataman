use std::{error::Error, path::Path};
use std::process::exit;
use regex::Regex;

use dataman::{controller::Controller, error::log, model::database::Database, tui::TUI};

fn main() -> Result<(), Box<dyn Error>> {

    // let re = Regex::new(r"(?<last>\w).*,\s(?<first>\w).*").unwrap();
    //
    // let result = re.replace_all("Springsteen aaa, Bruce", "$first $last");
    // let c = re.captures("Springsteen aaa, Bruce");
    // //show captures
    // println!("{:?}", c);
    // println!("{}", result);
    //
    //
    //
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
    let controller = Controller::new(tui, database);

    if let Err(err) = controller.start() {
        eprintln!("Program has quit due to error: {err}")
    }
    // clear screen
    // print!("\x1B[2J\x1B[1;1H");

    Ok(())
}
