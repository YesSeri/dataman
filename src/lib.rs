#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use clap::{command, Parser};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::fs;
use std::path::Path;

pub mod controller;
pub mod error;
pub mod io;
pub mod model;
pub mod navigation;
pub mod tui;

#[derive(Debug)]
pub struct Config {
    verbose: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeyBindings {
    /// Normalized keystroke (`ctrl+right`, `/`, `t`) to command name.
    pub(crate) bindings: std::collections::HashMap<String, String>,
}

fn load_keybindings() -> KeyBindings {
    let defaults = include_str!("../keybindings.toml");
    let file = Path::new("keybindings.toml");
    let config_content = fs::read_to_string(file).unwrap_or_else(|_| defaults.to_owned());
    toml::from_str(&config_content).unwrap_or_else(|err| {
        log::warn!("Could not parse keybindings.toml ({err}); using built-in defaults");
        toml::from_str(defaults).expect("built-in keybindings must be valid TOML")
    })
}

pub(crate) static KEYBINDINGS: Lazy<KeyBindings> = Lazy::new(load_keybindings);

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
