use kyori_component_json::Component;
use log::{debug, info, warn};
use minechat_protocol::{
    packets::MineChatPacket,
    protocol::{MessageStream, MineChatError, chat_format::COMMONMARK, chat_format::COMPONENTS},
    send_chat_message, send_disconnect, send_pong,
    types::MessageContent,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal;

static CHAT_FORMAT: &str = COMMONMARK;

struct ReplState {
    /// Whether the client will send outgoing messages as Minecraft text
    /// components. Only true when both the preferred format and the server
    /// capability agree.
    use_components: bool,
    /// Cached from the AUTH handshake so that `/format` can enforce it.
    server_supports_components: bool,
    /// Set to true when the server sends a MODERATION mute action.
    muted: bool,
}

impl ReplState {
    fn new(server_supports_components: bool) -> Self {
        Self {
            use_components: CHAT_FORMAT == COMPONENTS && server_supports_components,
            server_supports_components,
            muted: false,
        }
    }

    fn current_format(&self) -> &'static str {
        if self.use_components {
            COMPONENTS
        } else {
            COMMONMARK
        }
    }

    /// Toggle the outgoing chat format, respecting server capability.
    fn toggle_format(&mut self) {
        if !self.use_components && !self.server_supports_components {
            println!("Server does not support the 'components' format; staying on 'commonmark'.");
            return;
        }
        self.use_components = !self.use_components;
        println!(
            "Chat format switched to: {}",
            if self.use_components {
                "components"
            } else {
                "commonmark"
            }
        );
    }
}

pub async fn repl(
    stream: &mut (dyn MessageStream + Unpin + Send),
    server_supports_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut buffer = String::new();
    let mut state = ReplState::new(server_supports_components);

    loop {
        tokio::select! {
            result = stream.receive_packet() => {
                match result {
                    Ok(packet) => {
                        if handle_server_packet(packet, stream, &mut state).await? {
                            return Ok(());
                        }
                    }
                    Err(MineChatError::Disconnected) => {
                        info!("Server disconnected.");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!("Error receiving packet: {e}");
                        return Err(e.into());
                    }
                }
            }

            result = stdin.read_line(&mut buffer) => {
                let n = result?;

                // EOF - signal a clean exit
                if n == 0 {
                    send_disconnect(stream, "Client exit").await?;
                    return Ok(());
                }

                // Trim and clear immediately so we never forget to reset the buffer
                let input = buffer.trim().to_string();
                buffer.clear();

                if input.is_empty() {
                    continue;
                }

                if handle_input(stream, &input, &mut state).await? {
                    return Ok(());
                }
            }

            _ = signal::ctrl_c() => {
                send_disconnect(stream, "Client exit").await?;
                return Ok(());
            }
        }
    }
}

/// Process one line of user input.
///
/// Returns `true` if the REPL should exit after this call.
async fn handle_input(
    stream: &mut (dyn MessageStream + Unpin + Send),
    input: &str,
    state: &mut ReplState,
) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(cmd) = input.strip_prefix('/') {
        match cmd {
            "exit" => {
                send_disconnect(stream, "Client exit").await?;
                return Ok(true);
            }
            "format" => state.toggle_format(),
            _ => warn!("Unknown command: /{cmd}. Available commands: /format, /exit"),
        }
        return Ok(false);
    }

    // Plain chat message.
    if state.muted {
        println!("You are currently muted and cannot send messages.");
        return Ok(false);
    }

    let format = state.current_format();
    let content = if state.use_components {
        serde_json::to_string(&Component::text(input))?
    } else {
        input.to_owned()
    };
    send_chat_message(stream, format, &content).await?;

    Ok(false)
}

/// Dispatch an inbound packet from the server.
///
/// Returns `true` if the REPL should disconnect and exit.
async fn handle_server_packet(
    packet: MineChatPacket,
    stream: &mut (dyn MessageStream + Unpin + Send),
    state: &mut ReplState,
) -> Result<bool, Box<dyn std::error::Error>> {
    match packet {
        // Keep-alive - respond immediately
        MineChatPacket::Ping { timestamp_ms } => {
            debug!("Received PING ({timestamp_ms}), sending PONG.");
            send_pong(stream, timestamp_ms).await?;
        }

        MineChatPacket::ChatMessage { format, content } => {
            let text = match format.as_str() {
                COMMONMARK => match content {
                    MessageContent::CommonMark(ref t) => t.clone(),
                    _ => content.to_plain_text().to_string(),
                },
                COMPONENTS => match content {
                    MessageContent::Components(ref c) => c.to_plain_text().to_string(),
                    _ => content.to_plain_text().to_string(),
                },
                other => {
                    warn!("Unrecognised chat format '{other}', falling back to plain text.");
                    content.to_plain_text().to_string()
                }
            };
            println!("[Chat] {text}");
        }

        MineChatPacket::Moderation {
            action,
            scope,
            reason,
            duration_seconds,
        } => {
            let reason_str = reason.as_deref().unwrap_or("(no reason given)");

            match action.value() {
                // warn
                0 => {
                    warn!(
                        "Moderation: warn | scope={} | reason={reason_str}",
                        scope.value()
                    );
                    println!("[Warning] {reason_str}");
                }
                // mute
                1 => {
                    state.muted = true;
                    println!("[Muted] {reason_str}");
                    if let Some(secs) = duration_seconds {
                        println!("  Duration: {secs} seconds");
                    }
                }
                // kick
                2 => {
                    println!("[Kicked] {reason_str}");
                    return Ok(true);
                }
                // ban
                3 => {
                    println!("[Banned] {reason_str}");
                    return Ok(true);
                }
                other => {
                    warn!(
                        "Unknown moderation action {other} | scope={} | reason={reason_str}",
                        scope.value()
                    );
                }
            }
        }

        // Server-initiated disconnect
        MineChatPacket::Disconnect { reason } => {
            info!("Disconnected by server: {reason}");
            return Ok(true);
        }

        // Anything else - log at debug level and move on
        other => {
            debug!("Ignored unhandled packet: {other:?}");
        }
    }

    Ok(false)
}
