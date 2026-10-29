use std::io::Write;

use dataman::{controller::controller_impl::Controller, tui::TUI, Cli};
use env_logger::{Builder, Env};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    TUI::install_panic_hook();
    setup_logging();
    let cli = <Cli as clap::Parser>::parse();
    Controller::run_paths(cli.paths, TUI::new()?)
        .map_err(|error| color_eyre::eyre::Report::new(error).wrap_err("Dataman failed"))
}

fn setup_logging() {
    let env = Env::default();

    Builder::from_env(env)
        .write_style(env_logger::WriteStyle::Always)
        .format(|buf, record| {
            let timestamp = buf.timestamp_millis();
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
}
