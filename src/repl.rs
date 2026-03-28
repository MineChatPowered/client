use crate::connect::NetworkCommand;

use log::{debug, info, warn};

use minechat::{
    RustlsTlsMessageStream, message_content_to_ansi,
    packets::MineChatPacket,
    protocol::{
        MessageStream, MineChatError,
        chat_format::{COMMONMARK, COMPONENTS},
    },
    send_chat_message, send_pong,
    types::MessageContent,
};

use std::ops::ControlFlow;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, timeout};

struct ReplState {
    use_components: bool,
    muted: bool,
}

impl ReplState {
    fn new() -> Self {
        Self {
            use_components: false,
            muted: false,
        }
    }

    fn toggle_format(&mut self) {
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

async fn network_task(
    stream: Arc<Mutex<RustlsTlsMessageStream>>,
    packet_tx: mpsc::Sender<Result<MineChatPacket, MineChatError>>,
    mut send_rx: mpsc::Receiver<NetworkCommand>,
    mut shutdown_rx: mpsc::Receiver<()>,
) {
    loop {
        tokio::select! {
            result = async {
                let mut stream = stream.lock().await;
                stream.receive_packet().await
            } => {
                debug!("[NETWORK] Received packet: {:?}", result);
                match result {
                    Ok(packet) => {
                        if packet_tx.send(Ok(packet)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = packet_tx.send(Err(e)).await;
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(5)) => {
                // Small tick to check for outgoing commands
            }
            _ = shutdown_rx.recv() => {
                break;
            }
        }

        // Check for outgoing commands (non-blocking)
        while let Ok(msg) = send_rx.try_recv() {
            match msg {
                NetworkCommand::SendChat(content) => {
                    let mut stream = stream.lock().await;
                    let _ = send_chat_message(&mut *stream, COMMONMARK, &content).await;
                }
                NetworkCommand::SendPong(ts) => {
                    let mut stream = stream.lock().await;
                    let _ = send_pong(&mut *stream, ts).await;
                }
            }
        }
    }

    info!("Network task shutdown");
}

fn format_chat_message(format: &str, content: &MessageContent) -> String {
    match format {
        COMMONMARK => match content {
            MessageContent::CommonMark(t) => t.clone(),
            _ => content.to_plain_text().to_string(),
        },
        COMPONENTS => message_content_to_ansi(content),
        other => {
            warn!(
                "Unrecognised chat format '{}', falling back to plain text.",
                other
            );
            content.to_plain_text().to_string()
        }
    }
}

fn handle_moderation(
    action: i32,
    scope: i32,
    reason: &str,
    duration_seconds: Option<i32>,
) -> ControlFlow<()> {
    match action {
        0 => {
            warn!("Moderation: warn | scope={} | reason={}", scope, reason);
            println!("[Warning] {reason}");
            ControlFlow::Continue(())
        }
        1 => {
            println!("[Muted] {reason}");
            if let Some(secs) = duration_seconds {
                println!("  Duration: {secs} seconds");
            }
            ControlFlow::Continue(())
        }
        2 => {
            println!("[Kicked] {reason}");
            info!("Kicked from server: {}", reason);
            ControlFlow::Break(())
        }
        3 => {
            println!("[Banned] {reason}");
            info!("Banned from server: {}", reason);
            ControlFlow::Break(())
        }
        other => {
            warn!(
                "Unknown moderation action {} | scope={} | reason={}",
                other, scope, reason
            );
            ControlFlow::Continue(())
        }
    }
}

fn handle_packet(
    packet: MineChatPacket,
    send_tx: &mpsc::Sender<NetworkCommand>,
) -> ControlFlow<()> {
    match packet {
        MineChatPacket::Ping { timestamp_ms } => {
            debug!("Received PING ({timestamp_ms}), sending PONG.");
            if let Err(e) = send_tx.try_send(NetworkCommand::SendPong(timestamp_ms)) {
                warn!("Error sending pong: {}", e);
            }
            ControlFlow::Continue(())
        }
        MineChatPacket::ChatMessage { format, content } => {
            debug!(
                "[MAIN] Processing ChatMessage: format={:?}, content={:?}",
                format, content
            );
            let text = format_chat_message(format.as_str(), &content);
            println!("[Chat] {text}");
            ControlFlow::Continue(())
        }
        MineChatPacket::Moderation {
            action,
            scope,
            reason,
            duration_seconds,
        } => {
            let reason_str = reason.as_deref().unwrap_or("(no reason given)");
            handle_moderation(action.value(), scope.value(), reason_str, duration_seconds)
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
            info!("Disconnected by server: {}", reason_name);
            ControlFlow::Break(())
        }
        other => {
            debug!("Ignored unhandled packet: {:?}", other);
            ControlFlow::Continue(())
        }
    }
}

fn handle_stdin_command(
    cmd: &str,
    state: &mut ReplState,
    shutdown_tx: &mpsc::Sender<()>,
) -> ControlFlow<()> {
    match cmd {
        "exit" | "quit" => {
            println!("Exiting.");
            let _ = shutdown_tx.try_send(());
            ControlFlow::Break(())
        }
        "format" => {
            state.toggle_format();
            ControlFlow::Continue(())
        }
        "help" => {
            println!("Available commands: /exit, /quit, /format, /help");
            ControlFlow::Continue(())
        }
        _ => {
            println!(
                "Unknown command: /{}. Available commands: /exit, /quit, /format, /help",
                cmd
            );
            ControlFlow::Continue(())
        }
    }
}

fn handle_stdin_line(
    line: &str,
    state: &mut ReplState,
    send_tx: &mpsc::Sender<NetworkCommand>,
    shutdown_tx: &mpsc::Sender<()>,
) -> ControlFlow<()> {
    if line.is_empty() {
        info!("Exiting.");
        let _ = shutdown_tx.try_send(());
        return ControlFlow::Break(());
    }

    if let Some(cmd) = line.strip_prefix('/') {
        handle_stdin_command(cmd, state, shutdown_tx)
    } else if !state.muted {
        if let Err(e) = send_tx.try_send(NetworkCommand::SendChat(line.to_string())) {
            warn!("Error sending message: {}", e);
            println!("Error: {}", e);
        }
        ControlFlow::Continue(())
    } else {
        println!("You are currently muted and cannot send messages.");
        ControlFlow::Continue(())
    }
}

pub async fn repl(stream: RustlsTlsMessageStream) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = ReplState::new();

    println!("MineChat CLI - Type /help for commands, Ctrl+C to exit");
    println!("Tip: Use up/down arrows for history");

    let (packet_tx, mut packet_rx) = mpsc::channel(100);
    let (send_tx, send_rx) = mpsc::channel(32);
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

    let stream = Arc::new(Mutex::new(stream));
    let stream_for_network = Arc::clone(&stream);

    tokio::spawn(network_task(
        stream_for_network,
        packet_tx,
        send_rx,
        shutdown_rx,
    ));

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        tokio::select! {
            result = timeout(Duration::from_millis(100), packet_rx.recv()) => {
                match result {
                    Ok(Some(Ok(packet))) => {
                        debug!("[MAIN] Received packet: {:?}", packet);
                        if handle_packet(packet, &send_tx).is_break() {
                            break;
                        }
                    }
                    Ok(Some(Err(MineChatError::Disconnected))) => {
                        info!("Server disconnected.");
                        break;
                    }
                    Ok(Some(Err(e))) => {
                        warn!("Error receiving packet: {}", e);
                        return Err(e.into());
                    }
                    Ok(None) => {
                        info!("Network channel closed, exiting.");
                        break;
                    }
                    Err(_) => {
                        // timeout, continue
                    }
                }
            }
            result = reader.next_line() => {
                match result {
                    Ok(Some(line)) => {
                        if handle_stdin_line(&line, &mut state, &send_tx, &shutdown_tx).is_break() {
                            break;
                        }
                    }
                    Ok(None) => {
                        info!("EOF received, exiting.");
                        break;
                    }
                    Err(e) => {
                        debug!("Readline error: {}", e);
                        break;
                    }
                }
            }
        }
    }

    info!("REPL exiting");
    Ok(())
}
