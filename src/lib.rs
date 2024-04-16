#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use clap::{command, Parser};
use once_cell::sync::Lazy;

pub mod controller;
pub mod error;
pub mod model;
pub mod tui;

#[derive(Debug)]
pub struct Config {
    verbose: bool,
}

impl Config {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }
}

static CONFIG: Lazy<Config> = Lazy::new(|| {
    let cli_args = parse_cli_args();
    Config::new(cli_args.verbose)
});

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Parse csv files and explore them in a friendly TUI"
)]
pub struct Cli {
    pub paths: Vec<std::path::PathBuf>,

    #[arg(short, long)]
    pub verbose: bool,
}

fn parse_cli_args() -> Cli {
    Cli::parse()
}
