use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "Parse csv files and explore them in a friendly TUI"
)]
pub(crate) struct Cli {
    /// CSV file to use
    pub(crate) path: std::path::PathBuf,
    ///// Sets a custom config file
    //#[arg(short, long, value_name = "FILE")]
    //config: Option<PathBuf>,
    //
    ///// Turn debugging information on
    //#[arg(short, long, action = clap::ArgAction::Count)]
    //debug: u8,
    //
    //#[command(subcommand)]
    //command: Option<Commands>,
}

//#[derive(Subcommand, Debug)]
//enum Commands {
///// does testing things
//Test {
///// lists test values
//#[arg(short, long)]
//list: bool,
//},
//}
