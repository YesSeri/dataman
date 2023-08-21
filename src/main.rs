use std::path::PathBuf;

use dataman::{controller::Controller, view::BasicUI};

fn main() {
    let p = PathBuf::from("assets/data.csv");
    let mut c: Controller<BasicUI> = Controller::try_from(p).unwrap();
    c.run().unwrap();
}
