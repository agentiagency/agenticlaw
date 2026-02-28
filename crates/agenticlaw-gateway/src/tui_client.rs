//! TUI client mode — connects to a running agenticlaw gateway via WebSocket
//!
//! Shares rendering and key handling with tui.rs but uses a WS connection
//! instead of an embedded AgentRuntime.

use crate::tui::{draw, handle_key, App, VimMode};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use futures::{FutureExt, SinkExt, StreamExt};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMsg};

pub async fn run_tui_client(
    port: u16,
    session: String,
    token: Option<String>,
) -> anyhow::Result<()> {
    let url = format!("ws://127.0.0.1:{}/ws", port);

    let (ws_stream, _) = connect_async(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to gateway at {}: {}", url, e))?;

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Authenticate
    let auth_msg = serde_json::json!({ "token": token });
    ws_tx.send(WsMsg::Text(auth_msg.to_string())).await?;

    // Wait for auth response
    if let Some(Ok(WsMsg::Text(text))) = ws_rx.next().await {
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if v.get("event").and_then(|e| e.as_str()) == Some("info") {
            if let Some(Ok(WsMsg::Text(auth_text))) = ws_rx.next().await {
                let auth: serde_json::Value = serde_json::from_str(&auth_text).unwrap_or_default();
                if auth.get("event").and_then(|e| e.as_str()) == Some("auth")
                    && auth["data"]["ok"].as_bool() != Some(true)
                {
                    let err = auth["data"]["error"].as_str().unwrap_or("unknown");
                    anyhow::bail!("Authentication failed: {}", err);
                }
            }
        } else if v.get("event").and_then(|e| e.as_str()) == Some("auth")
            && v["data"]["ok"].as_bool() != Some(true)
        {
            let err = v["data"]["error"].as_str().unwrap_or("unknown");
            anyhow::bail!("Authentication failed: {}", err);
        }
    }

    let mut app = App::new("remote", &session, "(remote)");
    app.push_output(&format!("Connected to gateway at 127.0.0.1:{}\n", port));
    app.push_output(&format!("Session: {}\n\n", session));

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

    let mut req_id: u64 = 0;

    loop {
        terminal.draw(|f| draw(f, &app))?;

        let timeout = std::time::Duration::from_millis(16);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }

                if key.code == KeyCode::Esc && app.mode == VimMode::Normal && app.agent_running {
                    req_id += 1;
                    let abort = serde_json::json!({
                        "id": format!("req-{}", req_id),
                        "method": "chat.abort",
                        "params": { "session": session }
                    });
                    let _ = ws_tx.send(WsMsg::Text(abort.to_string())).await;
                    app.agent_running = false;
                    app.push_output("\n[cancelled]\n");
                    continue;
                }

                if let Some(message) = handle_key(&mut app, key) {
                    req_id += 1;
                    let rpc = serde_json::json!({
                        "id": format!("req-{}", req_id),
                        "method": "chat.send",
                        "params": {
                            "session": session,
                            "message": message
                        }
                    });
                    ws_tx.send(WsMsg::Text(rpc.to_string())).await?;
                    app.agent_running = true;
                }

                if app.should_quit {
                    break;
                }
            }
        }

        // Drain WS messages (non-blocking)
        loop {
            match ws_rx.next().now_or_never() {
                Some(Some(Ok(WsMsg::Text(text)))) => {
                    handle_ws_event(&mut app, &text);
                }
                Some(Some(Ok(WsMsg::Close(_)))) | Some(None) => {
                    app.push_output("\n[connection closed]\n");
                    app.should_quit = true;
                    break;
                }
                Some(Some(Err(e))) => {
                    app.push_output(&format!("\n[ws error: {}]\n", e));
                    app.should_quit = true;
                    break;
                }
                _ => break,
            }
        }

        if app.should_quit {
            break;
        }
    }

    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    let _ = ws_tx.send(WsMsg::Close(None)).await;

    Ok(())
}

fn handle_ws_event(app: &mut App, text: &str) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let event_type = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let data = &v["data"];

    match event_type {
        "chat" => {
            let msg_type = data["type"].as_str().unwrap_or("");
            match msg_type {
                "delta" => {
                    if let Some(content) = data["content"].as_str() {
                        app.push_output(content);
                    }
                }
                "tool_call" => {
                    let name = data["name"].as_str().unwrap_or("?");
                    app.push_output(&format!("\n[tool:{}]\n", name));
                }
                "tool_result" => {
                    let content = data["content"].as_str().unwrap_or("");
                    let is_error = data["is_error"].as_bool().unwrap_or(false);
                    if is_error {
                        app.push_output(&format!(
                            "  error: {}\n",
                            &content[..content.len().min(200)]
                        ));
                    } else {
                        app.push_output(&format!("  done ({} chars)\n", content.len()));
                    }
                }
                "done" => {
                    app.push_output("\n");
                    app.agent_running = false;
                }
                "error" => {
                    let msg = data["message"].as_str().unwrap_or("unknown error");
                    app.push_output(&format!("\nError: {}\n", msg));
                    app.agent_running = false;
                }
                _ => {}
            }
        }
        "info" => {
            if let Some(version) = data["version"].as_str() {
                app.model = format!("gateway v{}", version);
            }
        }
        _ => {}
    }
}
