use directories::ProjectDirs;
use kyori_component_json::Component;
use log::{debug, info, warn};
use minechat_protocol::protocol::{
    MessageStream, MineChatError,
    chat_format::{COMMONMARK, COMPONENTS},
};
use minechat_protocol::{
    packets::MineChatPacket, send_chat_message, send_pong, types::MessageContent,
};
use rustyline::DefaultEditor;
use std::path::PathBuf;
use tokio::signal;

struct ReplState {
    use_components: bool,
    server_supports_components: bool,
    muted: bool,
}

impl ReplState {
    fn new(server_supports_components: bool) -> Self {
        Self {
            use_components: false,
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

fn history_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "minechat").map(|dirs| dirs.data_dir().join("history"))
}

fn create_editor() -> rustyline::Result<DefaultEditor> {
    let mut editor = DefaultEditor::new()?;

    if let Some(history_path) = history_path() {
        if let Some(parent) = history_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Err(e) = editor.load_history(&history_path) {
            debug!("Could not load history: {}", e);
        }
    }

    Ok(editor)
}

pub async fn repl(
    stream: &mut (dyn MessageStream + Unpin + Send),
    server_supports_components: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut editor = create_editor()?;
    let mut state = ReplState::new(server_supports_components);

    println!("MineChat CLI - Type /help for commands, Ctrl+C to exit");
    println!("Tip: Use up/down arrows for history");

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

            _ = tokio::task::yield_now() => {
                let input = match editor.readline("> ") {
                    Ok(line) => line,
                    Err(rustyline::error::ReadlineError::Eof) => {
                        info!("Exiting.");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!("Readline error: {}", e);
                        continue;
                    }
                };

                if input.is_empty() {
                    continue;
                }

                if handle_input(stream, &input, &mut state).await? {
                    return Ok(());
                }

                let _ = editor.add_history_entry(&input);
            }

            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, exiting.");
                return Ok(());
            }
        }
    }
}

async fn handle_input(
    stream: &mut (dyn MessageStream + Unpin + Send),
    input: &str,
    state: &mut ReplState,
) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(cmd) = input.strip_prefix('/') {
        match cmd {
            "exit" | "quit" => {
                info!("Exiting.");
                return Ok(true);
            }
            "format" => state.toggle_format(),
            "help" => {
                println!("Available commands: /exit, /quit, /format, /help");
            }
            _ => warn!("Unknown command: /{cmd}. Available commands: /exit, /quit, /format, /help"),
        }
        return Ok(false);
    }

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

async fn handle_server_packet(
    packet: MineChatPacket,
    stream: &mut (dyn MessageStream + Unpin + Send),
    state: &mut ReplState,
) -> Result<bool, Box<dyn std::error::Error>> {
    match packet {
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
                0 => {
                    warn!(
                        "Moderation: warn | scope={} | reason={reason_str}",
                        scope.value()
                    );
                    println!("[Warning] {reason_str}");
                }
                1 => {
                    state.muted = true;
                    println!("[Muted] {reason_str}");
                    if let Some(secs) = duration_seconds {
                        println!("  Duration: {secs} seconds");
                    }
                }
                2 => {
                    println!("[Kicked] {reason_str}");
                    return Ok(true);
                }
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

        MineChatPacket::SystemDisconnect {
            reason_code,
            message,
        } => {
            let reason_name = match reason_code {
                0 => "Shutdown",
                1 => "Maintenance",
                2 => "Internal error",
                3 => "Overloaded",
                _ => "Unknown",
            };
            println!("[Server] Disconnected: {reason_name} - {message}");
            info!("Disconnected by server: {reason_name}");
            return Ok(true);
        }

        other => {
            debug!("Ignored unhandled packet: {other:?}");
        }
    }

    Ok(false)
}
