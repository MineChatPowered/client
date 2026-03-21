use clap::Parser;
use env_logger::{Builder, Target};
use log::{LevelFilter, info};

use crate::connect::{handle_connect, set_link};

mod config;
mod connect;
mod repl;

const DEFAULT_PORT: u16 = 7632;

#[derive(Parser)]
#[clap(
    name = "MineCLI",
    version = "0.2.0",
    author = "walker84837",
    about = "CLI client for MineChat"
)]
struct Args {
    /// The MineChat server host
    #[clap(required = true)]
    host: String,

    /// The MineChat server port (default: 7632)
    #[clap(short, long, default_value_t = DEFAULT_PORT)]
    port: u16,

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
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    });

    builder.init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let server_addr = format!("{}:{}", args.host, args.port);

    init_logger(args.verbose);

    info!("Connecting to {} (port {})", args.host, args.port);
    info!(
        "Chat format: {}",
        if args.components {
            "components"
        } else {
            "commonmark"
        }
    );

    if let Some(code) = args.link {
        set_link(&server_addr, &code, args.components).await?;
    } else {
        handle_connect(&server_addr, args.components).await?;
    }

    Ok(())
}
