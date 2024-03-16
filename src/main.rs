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
    use env_logger::{Builder, Env};
    use std::io::Write;
    let env = Env::default();

    Builder::from_env(env)
        .format(|buf, record| {
            // We are reusing `anstyle` but there are `anstyle-*` crates to adapt it to your
            // preferred styling crate.
            let timestamp = buf.timestamp();

            writeln!(
                buf,
                "[{timestamp} {} {}]: {}",
                record.line().unwrap_or(0),
                record.level(),
                record.args()
                )
        })
    .init();

    //env_logger::init();
    log::info!("a log from `MyLogger`");
}

