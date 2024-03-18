#![allow(unreachable_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
use dataman::{controller::Controller, error::AppError, model::database::Database, tui::TUI, Cli};
use log::{debug, error, info, trace, warn};

fn main() -> Result<(), AppError> {
    // if not release mode, print logs
    // if release mode, logs are not printed
    setup_logging();
    let cli = <Cli as clap::Parser>::parse();
    let path = cli.path;
    let database = Database::try_from(path).unwrap();
    let tui = TUI::new();
    let mut controller = Controller::new(tui, database);
    error!("This is an error");

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
        .write_style(env_logger::WriteStyle::Always).format(|buf, record| {
            // We are reusing `anstyle` but there are `anstyle-*` crates to adapt it to your
            // preferred styling crate.
            let timestamp = buf.timestamp();
            let write_style = buf.default_level_style(record.level());

            writeln!(
                buf,
                "[{} {write_style}{: >5}{write_style:#} {: >3}:{:<22}]: {}",
                timestamp,
                record.level(),
                record.line().unwrap_or(0),
                record.file().unwrap_or("file/???"),
                record.args(),
                )
        })
    .init();

    //env_logger::init();
    log::info!("a log from `MyLogger`");
}

