use ansi_term::Colour;
use clap::Parser;
use directories::ProjectDirs;
use env_logger::{Builder, Target};
use log::{LevelFilter, debug, info};
use miette::Result;
use minechat_protocol::{
    packets::{self, receive_message, send_message},
    protocol::{MineChatError, *},
};
use rustyline::{ExternalPrinter, history::DefaultHistory};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    path::PathBuf,
    thread,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader},
    net::TcpStream,
    signal,
    sync::mpsc,
};

#[derive(Parser)]
#[clap(
    name = "MineCLI",
    version = "0.1.1",
    author = "walker84837",
    about = "CLI client for MineChat",
    arg_required_else_help = true
)]
struct Args {
    /// The MineChat server hostname (without port)
    #[clap(short, long, value_parser = clap::builder::NonEmptyStringValueParser::new(), help = "The server hostname (e.g. example.com)")]
    server: String,

    /// The MineChat server port (defaults to 25575)
    #[clap(
        short = 'p',
        long,
        default_value = "25575",
        help = "The server port number"
    )]
    port: u16,

    /// Link account using the provided code
    #[clap(long, help = "Link account using the provided code")]
    link: Option<String>,

    /// Enable verbose logging
    #[clap(short, long, help = "Enable verbose logging")]
    verbose: bool,

    /// Connection timeout in seconds
    #[clap(
        long,
        default_value = "5",
        help = "Timeout (in seconds) for establishing a connection"
    )]
    timeout: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerConfig {
    servers: Vec<ServerEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ServerEntry {
    address: String,
    uuid: String,
}

fn config_path() -> Result<PathBuf, MineChatError> {
    let proj_dirs = ProjectDirs::from("org", "winlogon", "minechat")
        .ok_or(MineChatError::ConfigError("Can't get config dir".into()))?;
    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(config_dir.join("servers.json"))
}

fn load_config() -> Result<ServerConfig, MineChatError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(ServerConfig {
            servers: Vec::new(),
        });
    }
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}

fn save_config(config: &ServerConfig) -> Result<(), MineChatError> {
    let path = config_path()?;
    let file = File::create(path)?;
    Ok(serde_json::to_writer_pretty(file, config)?)
}

async fn set_link(server_addr: &str, code: &str) -> Result<(), MineChatError> {
    let (client_uuid, _link_code) = packets::link_with_server(server_addr, code).await?;

    info!("Linked successfully");
    let mut config = load_config()?;
    config.servers.retain(|e| e.address != server_addr);
    config.servers.push(ServerEntry {
        address: server_addr.to_string(),
        uuid: client_uuid,
    });
    save_config(&config)?;
    Ok(())
}

async fn handle_connect(server_addr: &str) -> Result<(), MineChatError> {
    let config = load_config()?;
    let entry = config
        .servers
        .iter()
        .find(|e| e.address == server_addr)
        .ok_or(MineChatError::ServerNotLinked)?;

    let mut stream = TcpStream::connect(server_addr).await?;
    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);

    send_message(
        &mut writer,
        &MineChatMessage::Auth {
            payload: AuthPayload {
                client_uuid: entry.uuid.clone(),
                link_code: String::new(),
            },
        },
    )
    .await?;

    match receive_message(&mut reader).await? {
        MineChatMessage::AuthAck { payload } => {
            if payload.status != "success" {
                return Err(MineChatError::AuthFailed(payload.message));
            }

            info!("Connected: {}", payload.message);
            let (reader, writer) = stream.into_split();
            repl(BufReader::new(reader), writer, server_addr.to_string()).await
        }
        _ => Err(MineChatError::AuthFailed("Unexpected response".into())),
    }
}

fn handle_incoming_message(
    result: std::io::Result<usize>,
    msg_buffer: &mut String,
    // TODO: make type alias for Whatever<rustyline::Editor<(), DefaultHistory>> -> SharedEditor
    rl: &mut rustyline::Editor<(), DefaultHistory>,
) -> Result<(), MineChatError> {
    let mut printer = rl.create_external_printer().unwrap();
    match result {
        Ok(0) => Ok(()),
        Ok(_) => {
            if let Ok(msg) = serde_json::from_str::<MineChatMessage>(msg_buffer) {
                match msg {
                    MineChatMessage::Broadcast { payload } => {
                        let formatted_message = format!(
                            "(MineChat) {}: {}",
                            Colour::Blue.paint(&payload.from),
                            Colour::Green.paint(&payload.message)
                        );
                        printer.print(formatted_message).unwrap();
                    }
                    MineChatMessage::Disconnect { payload } => {
                        printer
                            .print(
                                Colour::Red
                                    .paint(format!("Disconnected: {}", payload.reason))
                                    .to_string(),
                            )
                            .unwrap();
                        return Ok(());
                    }
                    _ => debug!("Received message: {:?}", msg),
                }
            }
            msg_buffer.clear();
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn repl<R, W>(mut reader: R, mut writer: W, server_name: String) -> Result<(), MineChatError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let config = config_path()?;
    let history_path = &config
        .parent()
        .ok_or(MineChatError::ConfigError("Invalid config path".into()))?
        .join("history.txt");

    let (tx, mut rx) = mpsc::channel::<String>(250);

    let history_path_clone = history_path.clone();

    // TODO: use types to allow this to be shared across theads, consider RwLock
    let mut rl = match rustyline::DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            return Err(MineChatError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )));
        }
    };

    thread::spawn(move || {
        if let Err(e) = rl.load_history(&history_path_clone) {
            eprintln!("Failed to load history: {}", e);
        }
        let prompt = format!(
            "MineChat ({}) >> ",
            Colour::Purple.paint(server_name.clone())
        );

        loop {
            match rl.readline(&prompt) {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        if let Err(e) = rl.add_history_entry(line.as_str()) {
                            eprintln!("Failed to add history entry: {}", e);
                        }
                    }
                    if tx.blocking_send(line).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("Readline error: {}", err);
                    break;
                }
            }
        }

        if let Err(e) = rl.save_history(&history_path_clone) {
            eprintln!("Failed to save history: {}", e);
        }
    });

    let mut msg_buffer = String::new();

    loop {
        tokio::select! {
            result = reader.read_line(&mut msg_buffer) => {
                handle_incoming_message(result, &mut msg_buffer, &mut rl)?;
            }
            // TODO: split this to a different function
            maybe_line = rx.recv() => {
                match maybe_line {
                    Some(line) => {
                        let input = line.trim().to_string();
                        // TODO: make an array of commands to leave
                        if input == "/exit" {
                            send_message(&mut writer, &MineChatMessage::Disconnect {
                                payload: DisconnectPayload { reason: "Client exit".into() }
                            }).await?;
                            return Ok(());
                        }
                        send_message(&mut writer, &MineChatMessage::Chat {
                            payload: ChatPayload { message: input }
                        }).await?;
                    }
                    None => break, // Channel closed
                }
            }
            _ = signal::ctrl_c() => {
                send_message(&mut writer, &MineChatMessage::Disconnect {
                    payload: DisconnectPayload { reason: "Client exit".into() }
                }).await?;
                return Ok(());
            }
        }
    }
    Ok(())
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
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logger(args.verbose);

    let server_addr = format!("{}:{}", args.server, args.port);

    if let Some(code) = args.link {
        set_link(&server_addr, &code).await
    } else {
        handle_connect(&server_addr).await
    }
    .map_err(miette::Report::new)?;

    Ok(())
}
