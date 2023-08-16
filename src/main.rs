use std::path::PathBuf;

use dataman::{view::BasicUI, controller::Controller};


fn main() {
    // Initialize your TUI framework here

    // Create an instance of the App struct
    let file_path = "assets/data.csv";
    let mut controller: Controller<BasicUI> = Controller::from(&PathBuf::from(file_path));
    controller.run();
    //app.derive(1, |cell| format!("{}X{}X{}", cell, cell, cell));

    //dbg!(app);
}
