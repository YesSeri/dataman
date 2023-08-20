use std::path::PathBuf;

use dataman::{controller::Controller, view::BasicUI};

fn main() {
    let p = PathBuf::from("assets/data.csv");
    let mut c: Controller<BasicUI> = Controller::from(&p);
    c.run();
}

//fn get_user_input() -> String {
//    use std::io::{self, Write};
//
//    let mut user_input = String::new();
//    print!("Enter a regex pattern: ");
//    io::stdout().flush().unwrap();
//
//    io::stdin()
//        .read_line(&mut user_input)
//        .expect("Failed to read user input");
//
//    user_input.trim().to_string()
//}
