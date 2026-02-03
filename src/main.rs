use clap::Parser;
use directories::ProjectDirs;
use env_logger::{Builder, Target};
use kyori_component_json::Component;
use log::{debug, info, warn};
use minechat_protocol::{
    RustlsTlsMessageStream, link_with_server,
    protocol::{chat_format::*, *},
    send_capabilities, send_chat_message, send_disconnect, send_pong, wait_auth_ok,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    marker::Send,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    signal,
};

// Global chat format state
static mut CHAT_FORMAT: &str = COMMONMARK;

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

#[derive(Debug, Deserialize, Serialize)]
struct ServerConfig {
    servers: Vec<ServerEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ServerEntry {
    address: String,
    client_uuid: String,
    minecraft_uuid: String,
    pinned_cert: Option<String>,
    supports_components: bool,
}

fn config_path() -> Result<String, Box<dyn std::error::Error>> {
    let proj_dirs = ProjectDirs::from("", "", "minechat").ok_or("Can't get config dir")?;
    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(config_dir
        .join("servers.json")
        .to_string_lossy()
        .to_string())
}

fn load_config() -> Result<ServerConfig, Box<dyn std::error::Error>> {
    let path = config_path()?;
    if !std::path::Path::new(&path).exists() {
        return Ok(ServerConfig {
            servers: Vec::new(),
        });
    }
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}

fn save_config(config: &ServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path()?;
    let file = File::create(path)?;
    Ok(serde_json::to_writer_pretty(file, config)?)
}

async fn set_link(server_addr: &str, code: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Validate link code
    if code.trim().is_empty() {
        return Err("Link code cannot be empty".into());
    }

    info!("Connecting to {} for linking...", server_addr);

    // Validate server address format
    let (host, port_str) = server_addr
        .rsplit_once(':')
        .ok_or("Invalid server address format. Expected format: host:port")?;

    // Validate port
    port_str
        .parse::<u16>()
        .map_err(|_| "Invalid port number. Must be between 1-65535")?;

    // Try to get existing pinned certificate
    let config = load_config()?;
    let pinned_cert = config
        .servers
        .iter()
        .find(|e| e.address == server_addr)
        .and_then(|e| e.pinned_cert.clone());

    // Connect with TLS and certificate pinning
    let mut message_stream = RustlsTlsMessageStream::connect_with_pinning(
        host,
        server_addr,
        pinned_cert.as_deref()
    ).await.map_err(|e| {
        match e {
            MineChatError::ConfigError(ref msg) if msg.contains("certificate") => {
                format!("Certificate verification failed: {}. This may indicate a MITM attack or server certificate change.", msg)
            }
            _ => format!("TLS connection failed: {}", e)
        }
    })?;

    info!("Sending LINK packet...");
    // For new linking, pass None so a new UUID is generated
    let (client_uuid, minecraft_uuid) = link_with_server(&mut message_stream, None, code).await?;

    info!("Linked successfully!");
    info!("Client UUID: {}", client_uuid);
    info!("Minecraft UUID: {}", minecraft_uuid);

    // Per spec: LINK -> LINK_OK -> CAPABILITIES -> AUTH_OK
    info!("Sending capabilities...");
    let supports_components = unsafe { CHAT_FORMAT == COMPONENTS };
    send_capabilities(&mut message_stream, supports_components).await?;

    // Wait for AUTH_OK
    wait_auth_ok(&mut message_stream).await?;
    info!("Authentication complete!");

    // Extract and store server certificate for pinning
    let pinned_cert = message_stream.server_certificate_base64();

    let mut config = load_config()?;
    config.servers.retain(|e| e.address != server_addr);
    config.servers.push(ServerEntry {
        address: server_addr.to_string(),
        client_uuid,
        minecraft_uuid,
        pinned_cert,
        supports_components: false, // Will be updated after capabilities exchange
    });
    save_config(&config)?;

    Ok(())
}

async fn handle_connect(server_addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    info!("Connecting to {}...", server_addr);

    let config = load_config()?;
    let entry = config
        .servers
        .iter()
        .find(|e| e.address == server_addr)
        .ok_or("Server not linked. Use --link to link first.")?;

    // Parse server address to separate host and port
    let (host, port_str) = server_addr
        .rsplit_once(':')
        .ok_or("Invalid server address format. Expected format: host:port")?;

    // Validate port
    port_str
        .parse::<u16>()
        .map_err(|_| "Invalid port number. Must be between 1-65535")?;

    // Connect with TLS and certificate pinning
    let mut message_stream = RustlsTlsMessageStream::connect_with_pinning(host, server_addr, entry.pinned_cert.as_deref()
    ).await.map_err(|e| {
        match e {
            MineChatError::ConfigError(ref msg) if msg.contains("certificate") => {
                format!("Certificate verification failed: {}. This may indicate a MITM attack or server certificate change.", msg)
            }
            _ => format!("TLS connection failed: {}", e)
        }
    })?;

    // Send reconnection packet with empty link code (per spec, empty = reconnection)
    // Pass the stored client_uuid so server can identify us
    info!("Authenticating with existing client UUID...");
    let (_client_uuid, _minecraft_uuid) =
        link_with_server(&mut message_stream, Some(entry.client_uuid.clone()), "").await?;

    // Send capabilities
    info!("Sending capabilities...");
    let use_components = unsafe { CHAT_FORMAT == COMPONENTS };
    send_capabilities(&mut message_stream, use_components).await?;

    // Wait for AUTH_OK per spec (CAPABILITIES -> AUTH_OK)
    wait_auth_ok(&mut message_stream).await?;

    // Update server entry with capabilities support after successful capabilities exchange
    let mut config = load_config()?;
    if let Some(server_entry) = config.servers.iter_mut().find(|e| e.address == server_addr) {
        server_entry.supports_components = use_components;
        save_config(&config)?;
    }

    info!(
        "Connected successfully! Type /exit to quit, /format to switch between CommonMark and components."
    );

    repl(
        &mut message_stream as &mut (dyn MessageStream + Unpin + Send),
        use_components,
    )
    .await
}

async fn repl(
    message_stream: &mut (dyn MessageStream + Unpin + Send),
    server_supports_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut buffer = String::new();
    let mut use_components = unsafe { CHAT_FORMAT == COMPONENTS } && server_supports_components;
    let mut muted = false;

    loop {
        tokio::select! {
            result = message_stream.receive_packet() => {
                match result {
                    Ok(packet) => {
                        // Handle PING immediately here (required by spec)
                        if let MineChatPacket {
                            packet_type: packet_types::PING,
                            payload: Payload::Ping(payload),
                        } = &packet {
                            debug!("Received PING: {}", payload.timestamp_ms);
                            send_pong(message_stream, payload.timestamp_ms).await?;
                            debug!("Sent PONG with timestamp: {}", payload.timestamp_ms);
                        }

                        let should_disconnect = handle_server_packet(packet, &mut muted).await?;
                        if should_disconnect {
                            return Ok(());
                        }
                    }
                    Err(MineChatError::Disconnected) => {
                        info!("Server disconnected");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!("Error receiving packet: {}", e);
                        return Err(e.into());
                    }
                }
            }
            result = stdin.read_line(&mut buffer) => {
                let n = result?;
                if n == 0 {
                    send_disconnect(message_stream, "Client exit").await?;
                    return Ok(());
                }
                let input = buffer.trim().to_string();
                if input.is_empty() {
                    buffer.clear();
                    continue;
                }

                if input == "/exit" {
                    send_disconnect(message_stream, "Client exit").await?;
                    return Ok(());
                } else if input.starts_with('/') {
                    if input == "/format" {
                        use_components = !use_components;
                        println!("Chat format switched to: {}",
                            if use_components { "components" } else { "commonmark" });
                    } else {
                        warn!("Unknown command: {}. Available commands: /format, /exit", input);
                    }
                } else {
                    // Per spec: enforce moderation - don't send if muted
                    if muted {
                        println!("You are currently muted and cannot send messages.");
                    } else {
                        let format = if use_components { COMPONENTS } else { COMMONMARK };
                        let content = if use_components {
                            // Create a simple text component
                            let component = Component::text(&input);
                            serde_json::to_string(&component)?
                        } else {
                            input.clone()
                        };
                        send_chat_message(message_stream, format, &content).await?;
                    }
                }
                buffer.clear();
            }
            _ = signal::ctrl_c() => {
                send_disconnect(message_stream, "Client exit").await?;
                return Ok(());
            }
        }
    }
}

async fn handle_server_packet(
    packet: MineChatPacket,
    muted: &mut bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    match packet {
        MineChatPacket {
            packet_type: packet_types::CHAT_MESSAGE,
            payload: Payload::ChatMessage(payload),
        } => match payload.format.as_str() {
            COMMONMARK => {
                println!("[Chat] {}", payload.content);
            }
            COMPONENTS => {
                // Parse and display component format
                match serde_json::from_str::<Component>(&payload.content) {
                    Ok(component) => {
                        println!("[Chat] {}", component.to_plain_text());
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse component: {}, falling back to raw content",
                            e
                        );
                        println!("[Chat] {}", payload.content);
                    }
                }
            }
            _ => {
                println!("[Chat] {} (format: {})", payload.content, payload.format);
            }
        },
        MineChatPacket {
            packet_type: packet_types::PING,
            payload: Payload::Ping(payload),
        } => {
            debug!("Received PING: {}", payload.timestamp_ms);
            // Note: PONG is handled in the main loop now
        }
        MineChatPacket {
            packet_type: packet_types::MODERATION,
            payload: Payload::Moderation(payload),
        } => {
            let action_str = match payload.action {
                moderation_action::WARN => "warn",
                moderation_action::MUTE => "mute",
                moderation_action::KICK => "kick",
                moderation_action::BAN => "ban",
                _ => "unknown",
            };
            let scope_str = match payload.scope {
                moderation_scope::CLIENT => "client",
                moderation_scope::ACCOUNT => "account",
                _ => "unknown",
            };

            // Per spec: Clients MUST enforce moderation actions locally
            match payload.action {
                moderation_action::WARN => {
                    warn!(
                        "Moderation warning: {} {}, reason: {:?}",
                        action_str, scope_str, payload.reason
                    );
                    println!(
                        "[Warning] {}",
                        payload.reason.as_deref().unwrap_or("You have been warned.")
                    );
                }
                moderation_action::MUTE => {
                    *muted = true;
                    println!("[Muted] You have been muted. Reason: {:?}", payload.reason);
                    if let Some(duration) = payload.duration_seconds {
                        println!("Duration: {} seconds", duration);
                    }
                }
                moderation_action::KICK => {
                    println!(
                        "[Kicked] You have been kicked. Reason: {:?}",
                        payload.reason
                    );
                    return Ok(true); // Signal to disconnect
                }
                moderation_action::BAN => {
                    println!(
                        "[Banned] You have been banned. Reason: {:?}",
                        payload.reason
                    );
                    return Ok(true); // Signal to disconnect
                }
                _ => {
                    warn!(
                        "Unknown moderation action: {} {}, reason: {:?}",
                        action_str, scope_str, payload.reason
                    );
                }
            }
        }
        MineChatPacket {
            packet_type: packet_types::DISCONNECT,
            payload: Payload::Disconnect(payload),
        } => {
            info!("Disconnected: {}", payload.reason);
            return Ok(true);
        }
        _ => {
            debug!("Received packet: {:?}", packet);
        }
    }
    Ok(false)
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

    // Set global chat format based on CLI flag
    unsafe {
        CHAT_FORMAT = if args.components {
            COMPONENTS
        } else {
            COMMONMARK
        };
    }

    if let Some(code) = args.link {
        set_link(&args.server, &code).await
    } else {
        handle_connect(&args.server).await
    }
}
