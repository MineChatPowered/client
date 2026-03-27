use crate::config::{ServerEntry, load_config, save_config};
use crate::repl::repl;
use log::{debug, info};
use minechat::{
    MessageStream, RustlsTlsMessageStream, link_with_server, protocol::MineChatError,
    send_capabilities, wait_auth_ok,
};

fn parse_server_addr(addr: &str) -> Result<&str, String> {
    let (host, port_str) = addr
        .rsplit_once(':')
        .ok_or("Invalid server address format. Expected format: host:port")?;

    port_str
        .parse::<u16>()
        .map_err(|_| "Invalid port number. Must be between 1-65535")?;

    Ok(host)
}

pub async fn set_link(
    server_addr: &str,
    code: &str,
    use_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if code.trim().is_empty() {
        return Err("Link code cannot be empty".into());
    }

    info!("Connecting to {} for linking...", server_addr);

    let host = parse_server_addr(server_addr)?;

    let config = load_config()?;
    let pinned_cert = config
        .servers
        .iter()
        .find(|e| e.address == server_addr)
        .and_then(|e| e.pinned_cert.clone());

    let mut message_stream = RustlsTlsMessageStream::connect_with_pinning(
        host,
        server_addr,
        pinned_cert.as_deref(),
    )
    .await
    .map_err(|e| match e {
        MineChatError::ConfigError(ref msg) if msg.contains("certificate") => {
            format!("Certificate verification failed: {msg}. This may indicate a MITM attack or server certificate change.")
        }
        _ => format!("TLS connection failed: {e}"),
    })?;

    info!("Sending LINK packet...");
    let (client_uuid, minecraft_uuid) = link_with_server(&mut message_stream, None, code).await?;

    info!("Linked successfully!");
    info!("Client UUID: {client_uuid}");
    info!("Minecraft UUID: {minecraft_uuid}");

    debug!("Sending capabilities...");
    let supported_formats = vec!["components".to_string(), "commonmark".to_string()];
    let preferred_format = if use_components {
        Some("components".to_string())
    } else {
        Some("commonmark".to_string())
    };
    send_capabilities(&mut message_stream, supported_formats, preferred_format).await?;

    wait_auth_ok(&mut message_stream).await?;
    info!("Authentication complete!");

    let pinned_cert = message_stream.server_certificate_base64();

    let mut config = load_config()?;
    config.servers.retain(|e| e.address != server_addr);
    config.servers.push(ServerEntry {
        address: server_addr.to_string(),
        client_uuid,
        minecraft_uuid,
        pinned_cert,
        supports_components: true, // Always support components now
    });
    save_config(&config)?;

    Ok(())
}

pub async fn handle_connect(
    server_addr: &str,
    use_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Connecting to {}...", server_addr);

    let host = parse_server_addr(server_addr)?;

    let mut config = load_config()?;
    let entry = config
        .servers
        .iter()
        .find(|e| e.address == server_addr)
        .ok_or("Server not linked. Use --link to link first.")?;

    let mut message_stream = RustlsTlsMessageStream::connect_with_pinning(
        host,
        server_addr,
        entry.pinned_cert.as_deref(),
    )
    .await
    .map_err(|e| match e {
        MineChatError::ConfigError(ref msg) if msg.contains("certificate") => {
            format!("Certificate verification failed: {msg}. This may indicate a MITM attack or server certificate change.")
        }
        _ => format!("TLS connection failed: {e}"),
    })?;

    info!("Authenticating with existing client UUID...");
    let (_client_uuid, _minecraft_uuid) =
        link_with_server(&mut message_stream, Some(entry.client_uuid.clone()), "").await?;

    info!("Sending capabilities...");
    let supported_formats = vec!["components".to_string(), "commonmark".to_string()];
    let preferred_format = if use_components {
        Some("components".to_string())
    } else {
        Some("commonmark".to_string())
    };
    send_capabilities(&mut message_stream, supported_formats, preferred_format).await?;

    wait_auth_ok(&mut message_stream).await?;

    if let Some(server_entry) = config.servers.iter_mut().find(|e| e.address == server_addr) {
        server_entry.supports_components = true; // Client always supports components now
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
