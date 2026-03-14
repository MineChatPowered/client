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

pub async fn repl(
    message_stream: &mut (dyn MessageStream + Unpin + Send),
    server_supports_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut buffer = String::new();
    let mut use_components = CHAT_FORMAT == COMPONENTS && server_supports_components;
    let mut muted = false;

    loop {
        tokio::select! {
            result = message_stream.receive_packet() => {
                match result {
                    Ok(packet) => {
                        if let MineChatPacket::Ping { timestamp_ms } = &packet {
                            debug!("Received PING: {}", timestamp_ms);
                            send_pong(message_stream, *timestamp_ms).await?;
                            debug!("Sent PONG with timestamp: {}", timestamp_ms);
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
                        println!(
                            "Chat format switched to: {}",
                            if use_components { "components" } else { "commonmark" }
                        );
                    } else {
                        warn!(
                            "Unknown command: {}. Available commands: /format, /exit",
                            input
                        );
                    }
                } else {
                    if muted {
                        println!("You are currently muted and cannot send messages.");
                    } else {
                        let format = if use_components {
                            COMPONENTS
                        } else {
                            COMMONMARK
                        };
                        let content = if use_components {
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
        MineChatPacket::ChatMessage { format, content } => match format.as_str() {
            COMMONMARK => {
                if let MessageContent::CommonMark(text) = content {
                    println!("[Chat] {}", text);
                } else {
                    println!("[Chat] {}", content.to_plain_text());
                }
            }
            COMPONENTS => {
                if let MessageContent::Components(component) = content {
                    println!("[Chat] {}", component.to_plain_text());
                } else {
                    println!("[Chat] {}", content.to_plain_text());
                }
            }
            _ => {
                println!(
                    "[Chat] {} (format: {})",
                    content.to_plain_text(),
                    format.as_str()
                );
            }
        },
        MineChatPacket::Ping { timestamp_ms } => {
            debug!("Received PING: {}", timestamp_ms);
        }
        MineChatPacket::Moderation {
            action,
            scope,
            reason,
            duration_seconds,
        } => {
            let action_val = action.value();
            let scope_val = scope.value();

            match action_val {
                0 => {
                    warn!(
                        "Moderation warning: action={}, scope={}, reason: {:?}",
                        action_val, scope_val, reason
                    );
                    println!(
                        "[Warning] {}",
                        reason.as_deref().unwrap_or("You have been warned.")
                    );
                }
                1 => {
                    *muted = true;
                    println!("[Muted] You have been muted. Reason: {:?}", reason);
                    if let Some(duration) = duration_seconds {
                        println!("Duration: {} seconds", duration);
                    }
                }
                2 => {
                    println!("[Kicked] You have been kicked. Reason: {:?}", reason);
                    return Ok(true);
                }
                3 => {
                    println!("[Banned] You have been banned. Reason: {:?}", reason);
                    return Ok(true);
                }
                _ => {
                    warn!(
                        "Unknown moderation action: action={}, scope={}, reason: {:?}",
                        action_val, scope_val, reason
                    );
                }
            }
        }
        MineChatPacket::Disconnect { reason } => {
            info!("Disconnected: {}", reason);
            return Ok(true);
        }
        _ => {
            debug!("Received packet: {:?}", packet);
        }
    }
    Ok(false)
}
