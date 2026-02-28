//! protectgateway — policy-enforcing reverse proxy
//!
//! Sits on :18789, forwards allowed requests to agenticlaw on :18790.
//! Intercepts tool_use JSON in WebSocket messages and enforces policy.

use crate::policy::{Decision, Policy};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite};
use tracing::{error, info, warn};

pub struct ProxyState {
    pub policy: Policy,
    pub upstream_url: String,
    pub upstream_ws_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolUse {
    #[serde(rename = "type")]
    msg_type: String,
    id: String,
    name: String,
    input: Value,
}

#[derive(Debug, Serialize)]
struct PolicyViolation {
    tool: String,
    decision: String,
    reason: String,
}

/// Create the protectgateway router.
pub fn create_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/policy", get(policy_handler))
        .with_state(state)
}

async fn health_handler(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "role": state.policy.role,
        "upstream": state.upstream_url,
    }))
}

async fn policy_handler(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    // Return the tool-level policy (not full details — don't leak filesystem/bash rules)
    Json(json!({
        "role": state.policy.role,
        "allowed_tools": state.policy.tools.allow,
        "denied_tools": state.policy.tools.deny,
    }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ProxyState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(client_socket: WebSocket, state: Arc<ProxyState>) {
    // Connect to upstream agenticlaw
    let upstream_url = format!("{}/ws", state.upstream_ws_url);
    let (upstream_ws, _) = match connect_async(&upstream_url).await {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to connect to upstream: {}", e);
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client_socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream_ws.split();

    let policy = state.policy.clone();

    // Use a channel so client_to_upstream can send denial messages back to the client
    let (denial_tx, mut denial_rx) = tokio::sync::mpsc::channel::<String>(32);

    // Client → (policy check) → Upstream
    let client_to_upstream = async move {
        while let Some(Ok(msg)) = client_rx.next().await {
            match msg {
                Message::Text(text) => {
                    match check_message(&policy, &text) {
                        MessageCheck::Allow => {
                            if upstream_tx
                                .send(tungstenite::Message::Text(text.into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        MessageCheck::Deny(violations) => {
                            let error_msg = json!({
                                "type": "policy_violation",
                                "violations": violations,
                            });
                            warn!("Policy violation: {:?}", violations);
                            let _ = denial_tx.send(error_msg.to_string()).await;
                        }
                        MessageCheck::Rewrite(rewritten) => {
                            if upstream_tx
                                .send(tungstenite::Message::Text(rewritten.into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                Message::Binary(data) => {
                    if upstream_tx
                        .send(tungstenite::Message::Binary(data.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    // Upstream → Client + denial messages → Client
    let upstream_to_client = async move {
        loop {
            tokio::select! {
                msg = upstream_rx.next() => {
                    match msg {
                        Some(Ok(tungstenite::Message::Text(text))) => {
                            if client_tx.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(tungstenite::Message::Binary(data))) => {
                            if client_tx.send(Message::Binary(data.into())).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(tungstenite::Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
                denial = denial_rx.recv() => {
                    match denial {
                        Some(msg) => {
                            if client_tx.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                        None => {} // channel closed, upstream task done
                    }
                }
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {},
        _ = upstream_to_client => {},
    }
}

enum MessageCheck {
    Allow,
    Deny(Vec<PolicyViolation>),
    Rewrite(String),
}

/// Check a JSON message for tool_use calls and enforce policy.
fn check_message(policy: &Policy, text: &str) -> MessageCheck {
    // Try to parse as JSON
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return MessageCheck::Allow, // Not JSON, pass through
    };

    // Look for tool_use content blocks
    let content = match value.get("content") {
        Some(Value::Array(arr)) => arr,
        _ => return MessageCheck::Allow,
    };

    let mut violations = Vec::new();
    let mut has_tool_use = false;

    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        has_tool_use = true;

        let tool_name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let input = block.get("input").cloned().unwrap_or(json!({}));

        let decision = policy.check_tool_call(tool_name, &input);
        match decision {
            Decision::Allow => {}
            Decision::Deny => {
                info!(
                    "DENIED tool_use: {} (role={})",
                    tool_name, policy.role
                );
                violations.push(PolicyViolation {
                    tool: tool_name.to_string(),
                    decision: "DENY".to_string(),
                    reason: format!(
                        "Tool '{}' is not permitted under {} policy",
                        tool_name, policy.role
                    ),
                });
            }
            Decision::Ask => {
                // In container mode, ask = log and allow (no HITL)
                warn!(
                    "ASK (auto-allowed) tool_use: {} (role={})",
                    tool_name, policy.role
                );
            }
        }
    }

    if !has_tool_use {
        return MessageCheck::Allow;
    }

    if violations.is_empty() {
        MessageCheck::Allow
    } else {
        MessageCheck::Deny(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_passes_through() {
        let policy = Policy::load("policies/READ.json").unwrap();
        assert!(matches!(check_message(&policy, "hello world"), MessageCheck::Allow));
    }

    #[test]
    fn allowed_tool_passes() {
        let policy = Policy::load("policies/READ.json").unwrap();
        let msg = json!({
            "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "read",
                "input": {"file_path": "/workspace/test.rs"}
            }]
        });
        assert!(matches!(check_message(&policy, &msg.to_string()), MessageCheck::Allow));
    }

    #[test]
    fn denied_tool_blocked() {
        let policy = Policy::load("policies/READ.json").unwrap();
        let msg = json!({
            "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "write",
                "input": {"file_path": "/workspace/evil.txt", "content": "pwned"}
            }]
        });
        assert!(matches!(check_message(&policy, &msg.to_string()), MessageCheck::Deny(_)));
    }
}
