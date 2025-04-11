use ansi_term::Colour;
use clap::Parser;
use directories::ProjectDirs;
use env_logger::{Builder, Target};
use log::{LevelFilter, debug, info};
use miette::Result;
use minechat_protocol::{
    packets::{self, send_message},
    protocol::{MineChatError, *},
};
use rustyline::{ExternalPrinter, error::ReadlineError, history::DefaultHistory};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Error as IoError, ErrorKind as IoErrorKind},
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader, ReadHalf, WriteHalf},
    net::TcpStream,
    signal,
    sync::mpsc,
};

const LEAVE_CMDS: &[&str] = &["/exit", "/quit", "/leave", "/disconnect"];

type SharedEditor = Arc<RwLock<rustyline::Editor<(), DefaultHistory>>>;

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

struct MineCLI {
    reader: BufReader<ReadHalf<TcpStream>>,
    writer: WriteHalf<TcpStream>,
    server_name: String,
    editor: SharedEditor,
    printer: Box<dyn ExternalPrinter>,
    history_path: PathBuf,
}

impl MineCLI {
    async fn connect(server_addr: impl AsRef<str>) -> Result<Self, MineChatError> {
        let server_addr = server_addr.as_ref();
        let config = load_config()?;
        let entry = config
            .servers
            .iter()
            .find(|e| e.address == server_addr)
            .ok_or(MineChatError::ServerNotLinked)?;

        let stream = TcpStream::connect(server_addr).await?;
        let (reader, mut writer) = tokio::io::split(stream);
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

        let mut msg_buffer = String::new();
        reader.read_line(&mut msg_buffer).await?;
        let msg = serde_json::from_str::<MineChatMessage>(&msg_buffer)?;

        match msg {
            MineChatMessage::AuthAck { payload } => {
                if payload.status != "success" {
                    return Err(MineChatError::AuthFailed(payload.message));
                }
                info!("Connected: {}", payload.message);
            }
            _ => return Err(MineChatError::AuthFailed("Unexpected response".into())),
        }

        let config_path = config_path()?;
        let history_path = config_path
            .parent()
            .ok_or(MineChatError::ConfigError("Invalid config path".into()))?
            .join("history.txt");

        let mut editor = rustyline::Editor::<(), DefaultHistory>::new()
            .map_err(|e| MineChatError::Io(IoError::new(IoErrorKind::Other, e)))?;
        let printer = editor
            .create_external_printer()
            .map_err(|e| MineChatError::Io(IoError::new(IoErrorKind::Other, e)))?;

        Ok(Self {
            reader,
            writer,
            server_name: server_addr.to_string(),
            editor: Arc::new(RwLock::new(editor)),
            printer: Box::new(printer),
            history_path,
        })
    }

    async fn run(mut self) -> Result<(), MineChatError> {
        let (tx, mut rx) = mpsc::channel::<String>(250);
        let editor_clone = self.editor.clone();
        let server_name = self.server_name.clone();
        let history_path = self.history_path.clone();

        thread::spawn(move || {
            let mut editor = match editor_clone.write() {
                Ok(guard) => guard,
                Err(e) => {
                    eprintln!("Failed to acquire editor lock: {}", e);
                    return;
                }
            };

            if let Err(e) = editor.load_history(&history_path) {
                eprintln!("Failed to load history: {}", e);
            }

            let prompt = format!(
                "MineChat ({}) >> ",
                Colour::Purple.paint(server_name.clone())
            );

            loop {
                let line = match editor.readline(&prompt) {
                    Ok(line) => line,
                    Err(ReadlineError::Interrupted) => continue,
                    Err(ReadlineError::Eof) => break,
                    Err(e) => {
                        eprintln!("Readline error: {}", e);
                        break;
                    }
                };

                if !line.trim().is_empty() {
                    if let Err(e) = editor.add_history_entry(line.as_str()) {
                        eprintln!("Failed to add history entry: {}", e);
                    }
                }

                if tx.blocking_send(line).is_err() {
                    break;
                }
            }

            if let Err(e) = editor.save_history(&history_path) {
                eprintln!("Failed to save history: {}", e);
            }
        });

        let mut msg_buffer = String::new();
        loop {
            tokio::select! {
                result = self.reader.read_line(&mut msg_buffer) => {
                    self.handle_incoming_message(result, &mut msg_buffer)?;
                }
                maybe_line = rx.recv() => {
                    if let Some(line) = maybe_line {
                        self.process_user_input(line).await?;
                    } else {
                        break;
                    }
                }
                _ = signal::ctrl_c() => {
                    self.send_disconnect().await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn handle_incoming_message(
        &mut self,
        result: std::io::Result<usize>,
        msg_buffer: &mut String,
    ) -> Result<(), MineChatError> {
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
                            self.printer.print(formatted_message).map_err(|e| {
                                MineChatError::Io(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    e.to_string(),
                                ))
                            })?;
                        }
                        MineChatMessage::Disconnect { payload } => {
                            self.printer
                                .print(
                                    Colour::Red
                                        .paint(format!("Disconnected: {}", payload.reason))
                                        .to_string(),
                                )
                                .map_err(|e| {
                                    MineChatError::Io(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        e.to_string(),
                                    ))
                                })?;
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

    async fn process_user_input(&mut self, input: String) -> Result<(), MineChatError> {
        let input = input.trim();

        if input.is_empty() {
            debug!("Received empty input message, skipping...");
            return Ok(());
        }

        if LEAVE_CMDS.contains(&input) {
            return self.send_disconnect().await;
        }

        send_message(
            &mut self.writer,
            &MineChatMessage::Chat {
                payload: ChatPayload {
                    message: input.to_string(),
                },
            },
        )
        .await
    }

    async fn send_disconnect(&mut self) -> Result<(), MineChatError> {
        send_message(
            &mut self.writer,
            &MineChatMessage::Disconnect {
                payload: DisconnectPayload {
                    reason: "Client exit".into(),
                },
            },
        )
        .await
    }
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
        let cli = MineCLI::connect(&server_addr).await?;
        cli.run().await
    }
    .map_err(miette::Report::new)?;

    Ok(())
}
