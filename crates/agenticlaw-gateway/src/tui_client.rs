//! Remote TUI client — connects to a running agenticlaw daemon via WebSocket
//!
//! This is the daemon-mode TUI: instead of embedding an AgentRuntime locally,
//! it connects to ws://host:port/ws and uses JSON-RPC to chat, while receiving
//! OutputEvents via the broadcast stream.

use crate::tui::{App, VimMode};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use futures::{SinkExt, StreamExt};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as TungMessage};

/// Events from the WebSocket connection to the TUI render loop.
#[derive(Debug)]
enum WsEvent {
    Delta {
        content: String,
    },
    ToolCall {
        name: String,
    },
    ToolResult {
        _name: String,
        len: usize,
        is_error: bool,
        snippet: String,
    },
    Done,
    Error {
        message: String,
    },
    Connected {
        version: String,
    },
    AuthResult {
        ok: bool,
        error: Option<String>,
    },
    Disconnected,
}

/// Run the TUI as a WebSocket client connected to a daemon.
pub async fn run_tui_remote(url: &str, session: &str, token: Option<&str>) -> anyhow::Result<()> {
    let ws_url = format!("{}/ws", url.trim_end_matches('/'));
    let parsed = url::Url::parse(&ws_url)?;

    // Connect to daemon
    let (ws_stream, _) = connect_async(parsed)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to daemon at {}: {}", ws_url, e))?;

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Authenticate
    let auth_msg = serde_json::json!({ "token": token });
    ws_tx.send(TungMessage::Text(auth_msg.to_string())).await?;

    // Channel for WS events → TUI
    let (event_tx, mut event_rx) = mpsc::channel::<WsEvent>(256);

    // Channel for TUI → WS sender
    let (send_tx, mut send_rx) = mpsc::channel::<String>(64);

    // Spawn WS reader task
    let reader_tx = event_tx.clone();
    let session_filter = session.to_string();
    tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(TungMessage::Text(text)) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(evt) = parse_ws_event(&json, &session_filter) {
                            if reader_tx.send(evt).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Ok(TungMessage::Close(_)) | Err(_) => {
                    let _ = reader_tx.send(WsEvent::Disconnected).await;
                    break;
                }
                _ => {}
            }
        }
    });

    // Spawn WS writer task
    tokio::spawn(async move {
        while let Some(json_str) = send_rx.recv().await {
            if ws_tx.send(TungMessage::Text(json_str)).await.is_err() {
                break;
            }
        }
    });

    // Setup terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new("daemon", session, "remote");
    app.push_output(&format!("Connecting to {}...\n", url));

    let mut req_id: u64 = 0;
    let session_name = session.to_string();

    // Event loop
    loop {
        terminal.draw(|f| crate::tui::draw_pub(f, &app))?;

        let timeout = std::time::Duration::from_millis(16);

        // Check terminal events
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                // Ctrl-C always quits
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }

                // ESC in normal mode while running = abort
                if key.code == KeyCode::Esc && app.mode == VimMode::Normal && app.agent_running {
                    req_id += 1;
                    let abort_msg = serde_json::json!({
                        "id": format!("req-{}", req_id),
                        "method": "chat.abort",
                        "params": { "session": session_name }
                    });
                    let _ = send_tx.send(abort_msg.to_string()).await;
                    app.agent_running = false;
                    app.push_output("\n[cancelled]\n");
                    continue;
                }

                if let Some(message) = handle_key_remote(&mut app, key) {
                    if app.agent_running {
                        // Steering: send as another chat.send (the daemon handles queuing)
                        req_id += 1;
                        let steer_msg = serde_json::json!({
                            "id": format!("req-{}", req_id),
                            "method": "chat.send",
                            "params": {
                                "session": session_name,
                                "message": message
                            }
                        });
                        let _ = send_tx.send(steer_msg.to_string()).await;
                        app.push_output(&format!(
                            "\n> {}\n[steering — interrupting agent]\n\n",
                            message.trim()
                        ));
                    } else {
                        // Normal send
                        app.agent_running = true;
                        app.push_output(&format!("\n> {}\n\n", message.trim()));
                        req_id += 1;
                        let chat_msg = serde_json::json!({
                            "id": format!("req-{}", req_id),
                            "method": "chat.send",
                            "params": {
                                "session": session_name,
                                "message": message
                            }
                        });
                        let _ = send_tx.send(chat_msg.to_string()).await;
                    }
                }

                if app.should_quit {
                    break;
                }
            }
        }

        // Drain WS events
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                WsEvent::Connected { version } => {
                    app.push_output(&format!("Connected to agenticlaw daemon v{}\n\n", version));
                }
                WsEvent::AuthResult { ok, error } => {
                    if ok {
                        app.push_output("[authenticated]\n");
                    } else {
                        app.push_output(&format!(
                            "[auth failed: {}]\n",
                            error.as_deref().unwrap_or("unknown")
                        ));
                    }
                }
                WsEvent::Delta { content } => {
                    app.push_output(&content);
                }
                WsEvent::ToolCall { name } => {
                    app.push_output(&format!("\n[tool:{}]\n", name));
                }
                WsEvent::ToolResult {
                    _name: _,
                    len,
                    is_error,
                    snippet,
                } => {
                    if is_error {
                        app.push_output(&format!("  error: {}\n", snippet));
                    } else {
                        app.push_output(&format!("  done ({} chars)\n", len));
                    }
                }
                WsEvent::Done => {
                    app.push_output("\n");
                    app.agent_running = false;
                }
                WsEvent::Error { message } => {
                    app.push_output(&format!("\nError: {}\n", message));
                    app.agent_running = false;
                }
                WsEvent::Disconnected => {
                    app.push_output("\n[disconnected from daemon]\n");
                    app.agent_running = false;
                }
            }
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    eprintln!("Disconnected from daemon, goodbye!");
    Ok(())
}

/// Parse a WebSocket JSON message into a WsEvent.
fn parse_ws_event(json: &serde_json::Value, session_filter: &str) -> Option<WsEvent> {
    // Info event (sent on connect)
    if json.get("event").and_then(|e| e.as_str()) == Some("info") {
        let version = json["data"]["version"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        return Some(WsEvent::Connected { version });
    }

    // Auth event
    if json.get("event").and_then(|e| e.as_str()) == Some("auth") {
        let ok = json["data"]["ok"].as_bool().unwrap_or(false);
        let error = json["data"]["error"].as_str().map(String::from);
        return Some(WsEvent::AuthResult { ok, error });
    }

    // Chat events — filter by session
    if json.get("event").and_then(|e| e.as_str()) == Some("chat") {
        let data = &json["data"];
        let session = data["session"].as_str().unwrap_or("");
        if session != session_filter {
            return None; // Not our session
        }

        let event_type = data["type"].as_str().unwrap_or("");
        match event_type {
            "delta" => {
                let content = data["content"].as_str().unwrap_or("").to_string();
                Some(WsEvent::Delta { content })
            }
            "tool_call" => {
                let name = data["name"].as_str().unwrap_or("unknown").to_string();
                Some(WsEvent::ToolCall { name })
            }
            "tool_result" => {
                let name = data["name"].as_str().unwrap_or("").to_string();
                let content = data["content"].as_str().unwrap_or("");
                let is_error = data["is_error"].as_bool().unwrap_or(false);
                let len = content.len();
                let snippet = content[..content.len().min(200)].to_string();
                Some(WsEvent::ToolResult {
                    _name: name,
                    len,
                    is_error,
                    snippet,
                })
            }
            "done" => Some(WsEvent::Done),
            "error" => {
                let message = data["message"].as_str().unwrap_or("unknown").to_string();
                Some(WsEvent::Error { message })
            }
            _ => None,
        }
    } else {
        None
    }
}

/// Key handler for remote TUI — reuses the same App state but returns messages to send.
/// This is a simplified version that delegates to the existing key handling in tui.rs.
fn handle_key_remote(app: &mut App, key: KeyEvent) -> Option<String> {
    // Ctrl-L toggles log panel
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
        app.show_logs = !app.show_logs;
        return None;
    }

    match app.mode {
        VimMode::Normal => handle_normal_key_remote(app, key),
        VimMode::Insert => handle_insert_key_remote(app, key),
    }
}

fn handle_normal_key_remote(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Esc => None,
        KeyCode::Enter => {
            let text = app.editor_lines.join("\n");
            if !text.trim().is_empty() {
                app.editor_lines = vec![String::new()];
                app.cursor_row = 0;
                app.cursor_col = 0;
                return Some(text);
            }
            None
        }
        KeyCode::Char('i') => {
            app.mode = VimMode::Insert;
            None
        }
        KeyCode::Char('a') => {
            app.mode = VimMode::Insert;
            let len = app.editor_lines[app.cursor_row].chars().count();
            app.cursor_col = (app.cursor_col + 1).min(len);
            None
        }
        KeyCode::Char('A') => {
            app.mode = VimMode::Insert;
            app.cursor_col = app.editor_lines[app.cursor_row].chars().count();
            None
        }
        KeyCode::Char('I') => {
            app.mode = VimMode::Insert;
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('o') => {
            app.mode = VimMode::Insert;
            let new_row = app.cursor_row + 1;
            app.editor_lines.insert(new_row, String::new());
            app.cursor_row = new_row;
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('h') | KeyCode::Left => {
            app.cursor_col = app.cursor_col.saturating_sub(1);
            None
        }
        KeyCode::Char('l') | KeyCode::Right => {
            let max = app.editor_lines[app.cursor_row]
                .chars()
                .count()
                .saturating_sub(1);
            app.cursor_col = (app.cursor_col + 1).min(max);
            None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.cursor_row + 1 < app.editor_lines.len() {
                app.cursor_row += 1;
            }
            app.clamp_cursor();
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.cursor_row = app.cursor_row.saturating_sub(1);
            app.clamp_cursor();
            None
        }
        KeyCode::Char('0') => {
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('$') => {
            app.cursor_col = app.editor_lines[app.cursor_row]
                .chars()
                .count()
                .saturating_sub(1)
                .max(0);
            None
        }
        KeyCode::Char('G') => {
            app.output_scroll = app.output_lines.len();
            None
        }
        KeyCode::Char('g') => {
            app.output_scroll = 0;
            None
        }
        KeyCode::Char('q') if !app.agent_running => {
            app.should_quit = true;
            None
        }
        KeyCode::Char('x') => {
            let len = app.editor_lines[app.cursor_row].chars().count();
            if len > 0 && app.cursor_col < len {
                let byte_off = char_to_byte(&app.editor_lines[app.cursor_row], app.cursor_col);
                app.editor_lines[app.cursor_row].remove(byte_off);
                app.clamp_cursor();
            }
            None
        }
        KeyCode::Char('d') => {
            if app.editor_lines.len() > 1 {
                app.editor_lines.remove(app.cursor_row);
                app.clamp_cursor();
            } else {
                app.editor_lines[0].clear();
                app.cursor_col = 0;
            }
            None
        }
        _ => None,
    }
}

fn handle_insert_key_remote(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Esc => {
            app.mode = VimMode::Normal;
            app.clamp_cursor();
            None
        }
        KeyCode::Enter => {
            let byte_off = char_to_byte(&app.editor_lines[app.cursor_row], app.cursor_col);
            let rest = app.editor_lines[app.cursor_row][byte_off..].to_string();
            app.editor_lines[app.cursor_row].truncate(byte_off);
            app.cursor_row += 1;
            app.editor_lines.insert(app.cursor_row, rest);
            app.cursor_col = 0;
            None
        }
        KeyCode::Backspace => {
            if app.cursor_col > 0 {
                let prev_byte = char_to_byte(&app.editor_lines[app.cursor_row], app.cursor_col - 1);
                app.editor_lines[app.cursor_row].remove(prev_byte);
                app.cursor_col -= 1;
            } else if app.cursor_row > 0 {
                let line = app.editor_lines.remove(app.cursor_row);
                app.cursor_row -= 1;
                app.cursor_col = app.editor_lines[app.cursor_row].chars().count();
                app.editor_lines[app.cursor_row].push_str(&line);
            }
            None
        }
        KeyCode::Char(c) => {
            let byte_off = char_to_byte(&app.editor_lines[app.cursor_row], app.cursor_col);
            app.editor_lines[app.cursor_row].insert(byte_off, c);
            app.cursor_col += 1;
            None
        }
        KeyCode::Left => {
            app.cursor_col = app.cursor_col.saturating_sub(1);
            None
        }
        KeyCode::Right => {
            let len = app.editor_lines[app.cursor_row].chars().count();
            app.cursor_col = (app.cursor_col + 1).min(len);
            None
        }
        KeyCode::Up => {
            app.cursor_row = app.cursor_row.saturating_sub(1);
            app.clamp_cursor();
            None
        }
        KeyCode::Down => {
            if app.cursor_row + 1 < app.editor_lines.len() {
                app.cursor_row += 1;
            }
            app.clamp_cursor();
            None
        }
        _ => None,
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_info_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "info",
            "data": { "version": "0.5.0", "layer": null }
        });
        match parse_ws_event(&json, "test") {
            Some(WsEvent::Connected { version }) => assert_eq!(version, "0.5.0"),
            other => panic!("Expected Connected, got {:?}", other),
        }
    }

    #[test]
    fn parse_auth_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "auth",
            "data": { "ok": true, "error": null }
        });
        match parse_ws_event(&json, "test") {
            Some(WsEvent::AuthResult { ok, error }) => {
                assert!(ok);
                assert!(error.is_none());
            }
            other => panic!("Expected AuthResult, got {:?}", other),
        }
    }

    #[test]
    fn parse_delta_event_matching_session() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "main", "type": "delta", "content": "hello" }
        });
        match parse_ws_event(&json, "main") {
            Some(WsEvent::Delta { content }) => assert_eq!(content, "hello"),
            other => panic!("Expected Delta, got {:?}", other),
        }
    }

    #[test]
    fn parse_delta_event_wrong_session_filtered() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "other", "type": "delta", "content": "hello" }
        });
        assert!(parse_ws_event(&json, "main").is_none());
    }

    #[test]
    fn parse_done_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "s1", "type": "done" }
        });
        match parse_ws_event(&json, "s1") {
            Some(WsEvent::Done) => {}
            other => panic!("Expected Done, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_call_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "s1", "type": "tool_call", "id": "t1", "name": "bash" }
        });
        match parse_ws_event(&json, "s1") {
            Some(WsEvent::ToolCall { name }) => assert_eq!(name, "bash"),
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn parse_error_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "s1", "type": "error", "message": "oops" }
        });
        match parse_ws_event(&json, "s1") {
            Some(WsEvent::Error { message }) => assert_eq!(message, "oops"),
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_result_event() {
        let json: serde_json::Value = serde_json::json!({
            "event": "chat",
            "data": { "session": "s1", "type": "tool_result", "id": "t1", "name": "bash", "content": "output here", "is_error": false }
        });
        match parse_ws_event(&json, "s1") {
            Some(WsEvent::ToolResult {
                _name,
                len,
                is_error,
                ..
            }) => {
                assert_eq!(_name, "bash");
                assert_eq!(len, 11);
                assert!(!is_error);
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }
}
