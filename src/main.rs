use clap::Parser;
use env_logger::{Builder, Target};
use log::info;

use crate::connect::{handle_connect, set_link};

mod config;
mod connect;
mod repl;

#[derive(Parser)]
#[clap(
    name = "MineCLI",
    version = "0.2.0",
    author = "walker84837",
    about = "CLI client for MineChat Protocol v1.0.0"
)]
struct Args {
    /// The MineChat server address (host:port)
    #[clap(short, long, required = true)]
    server: String,

    /// Link account using the provided code
    #[clap(long)]
    link: Option<String>,

    /// Enable verbose logging
    #[clap(short, long)]
    verbose: bool,

    /// Use component format instead of CommonMark
    #[clap(long)]
    components: bool,
}

fn init_logger(verbose: bool) {
    let mut builder = Builder::from_default_env();
    builder.target(Target::Stdout);
    builder.filter_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    builder.init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    init_logger(args.verbose);

    info!(
        "Chat format: {}",
        if args.components {
            "components"
        } else {
            "commonmark"
        }
    );

    if let Some(code) = args.link {
        set_link(&args.server, &code, args.components).await
    } else {
        handle_connect(&args.server, args.components).await
    }
}
