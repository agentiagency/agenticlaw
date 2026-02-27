//! WebSocket connection handling with v3 RPC protocol
//!
//! Handles both the new JSON-RPC protocol (IncomingMessage) and streams
//! OutputEvents to connected clients via broadcast subscription.

use crate::auth::ResolvedAuth;
use crate::rpc::{self, ConnectionContext, output_event_to_message};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use agenticlaw_agent::{AgentRuntime, OutputEvent};
use agenticlaw_core::{EventMessage, IncomingMessage, RpcResponse};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Shared state for WebSocket connections.
pub struct WsState {
    pub auth: ResolvedAuth,
    pub agent: Arc<AgentRuntime>,
    pub layer: Option<String>,
    pub port: u16,
    /// Broadcast channel for OutputEvents — all WS clients subscribe.
    pub output_tx: broadcast::Sender<OutputEvent>,
    /// Whether consciousness stack is enabled alongside this gateway.
    pub consciousness_enabled: bool,
    /// When the gateway started.
    pub started_at: std::time::Instant,
}

/// Handle a WebSocket connection using the v3 RPC protocol.
pub async fn handle_connection(socket: WebSocket, state: Arc<WsState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Subscribe to output events
    let mut output_rx = state.output_tx.subscribe();

    // Send info event on connect
    let info_event = EventMessage::info(env!("CARGO_PKG_VERSION"), state.layer.as_deref());
    if let Ok(json) = serde_json::to_string(&info_event) {
        let _ = ws_tx.send(WsMessage::Text(json)).await;
    }

    let mut authenticated = false;

    // Connection context for RPC handlers
    let ctx = ConnectionContext {
        authenticated: false,
        agent: state.agent.clone(),
        output_tx: state.output_tx.clone(),
    };

    loop {
        tokio::select! {
            // Handle incoming WebSocket messages
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        let responses = handle_text_message(
                            &text,
                            &state,
                            &mut authenticated,
                            &ctx,
                        ).await;

                        for response_json in responses {
                            if ws_tx.send(WsMessage::Text(response_json)).await.is_err() {
                                return; // Client disconnected
                            }
                        }
                    }
                    Some(Ok(WsMessage::Ping(_))) => {
                        let pong = EventMessage::pong();
                        if let Ok(json) = serde_json::to_string(&pong) {
                            let _ = ws_tx.send(WsMessage::Text(json)).await;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => {
                        info!("Client disconnected");
                        return;
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        return;
                    }
                    None => return, // Stream ended
                    _ => {} // Binary, Pong — ignore
                }
            }

            // Forward OutputEvents to the client
            event = output_rx.recv() => {
                match event {
                    Ok(output_event) => {
                        let event_msg = output_event_to_message(&output_event);
                        if let Ok(json) = serde_json::to_string(&event_msg) {
                            if ws_tx.send(WsMessage::Text(json)).await.is_err() {
                                return; // Client disconnected
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Client lagged, dropped {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("Output broadcast closed");
                        return;
                    }
                }
            }
        }
    }
}

/// Handle a text message. Returns JSON strings to send back to the client.
async fn handle_text_message(
    text: &str,
    state: &Arc<WsState>,
    authenticated: &mut bool,
    ctx: &ConnectionContext,
) -> Vec<String> {
    let mut responses = Vec::new();

    // Try parsing as the new v3 protocol first
    match serde_json::from_str::<IncomingMessage>(text) {
        Ok(IncomingMessage::Rpc(req)) => {
            // Handle auth RPC specially
            if req.method == "auth" {
                let token = req.params["token"].as_str();
                match state.auth.verify_token(token) {
                    Ok(()) => {
                        *authenticated = true;
                        let resp = RpcResponse::ok(&req.id, serde_json::json!({ "ok": true }));
                        if let Ok(json) = serde_json::to_string(&resp) {
                            responses.push(json);
                        }
                        info!("Client authenticated (RPC)");
                    }
                    Err(e) => {
                        let resp = RpcResponse::auth_error(&req.id, e.to_string());
                        if let Ok(json) = serde_json::to_string(&resp) {
                            responses.push(json);
                        }
                        warn!("Auth failed: {}", e);
                    }
                }
                return responses;
            }

            // Route to RPC handler
            let rpc_ctx = ConnectionContext {
                authenticated: *authenticated,
                agent: ctx.agent.clone(),
                output_tx: ctx.output_tx.clone(),
            };
            let result = rpc::route_rpc(&req.method, req.params, &rpc_ctx).await;
            let resp = rpc::to_response(&req.id, result);
            if let Ok(json) = serde_json::to_string(&resp) {
                responses.push(json);
            }
        }

        Ok(IncomingMessage::Auth { token }) => {
            // Auth shorthand
            match state.auth.verify_token(token.as_deref()) {
                Ok(()) => {
                    *authenticated = true;
                    let evt = EventMessage::auth_result(true, None);
                    if let Ok(json) = serde_json::to_string(&evt) {
                        responses.push(json);
                    }
                    info!("Client authenticated (shorthand)");
                }
                Err(e) => {
                    let evt = EventMessage::auth_result(false, Some(&e.to_string()));
                    if let Ok(json) = serde_json::to_string(&evt) {
                        responses.push(json);
                    }
                    warn!("Auth failed: {}", e);
                }
            }
        }

        Err(_) => {
            // Try legacy v2 protocol as fallback
            if let Ok(legacy) = serde_json::from_str::<agenticlaw_core::ClientMessage>(text) {
                let legacy_responses = handle_legacy_message(legacy, state, authenticated).await;
                responses.extend(legacy_responses);
            } else {
                warn!("Unparseable message: {}", &text[..text.len().min(100)]);
            }
        }
    }

    responses
}

/// Handle a legacy v2 protocol message. Returns JSON strings to send back.
async fn handle_legacy_message(
    msg: agenticlaw_core::ClientMessage,
    state: &Arc<WsState>,
    authenticated: &mut bool,
) -> Vec<String> {
    use agenticlaw_core::{ClientMessage, ServerMessage};
    let mut responses = Vec::new();

    match msg {
        ClientMessage::Auth { token } => {
            match state.auth.verify_token(token.as_deref()) {
                Ok(()) => {
                    *authenticated = true;
                    if let Ok(json) = serde_json::to_string(&ServerMessage::auth_ok()) {
                        responses.push(json);
                    }
                    info!("Client authenticated (legacy)");
                }
                Err(e) => {
                    if let Ok(json) = serde_json::to_string(&ServerMessage::auth_failed(e.to_string())) {
                        responses.push(json);
                    }
                    warn!("Auth failed: {}", e);
                }
            }
        }
        ClientMessage::Chat { session, message, model } => {
            if !*authenticated {
                if let Ok(json) = serde_json::to_string(&ServerMessage::auth_failed("not authenticated")) {
                    responses.push(json);
                }
                return responses;
            }

            // Use the v3 RPC handler under the hood
            let ctx = ConnectionContext {
                authenticated: true,
                agent: state.agent.clone(),
                output_tx: state.output_tx.clone(),
            };
            let mut params = serde_json::json!({ "session": session, "message": message });
            if let Some(m) = model {
                params["model"] = serde_json::Value::String(m);
            }
            let _ = rpc::route_rpc("chat.send", params, &ctx).await;
            // Events stream via broadcast — no direct response needed for legacy
        }
        ClientMessage::Abort { session } => {
            let session_key = agenticlaw_agent::SessionKey::new(&session);
            if let Some(sess) = state.agent.sessions().get(&session_key) {
                sess.abort().await;
            }
        }
        ClientMessage::Call { id, method, params } => {
            if !*authenticated {
                if let Ok(json) = serde_json::to_string(&ServerMessage::result_error(&id, "not authenticated")) {
                    responses.push(json);
                }
                return responses;
            }

            let ctx = ConnectionContext {
                authenticated: true,
                agent: state.agent.clone(),
                output_tx: state.output_tx.clone(),
            };
            let result = rpc::route_rpc(&method, params, &ctx).await;
            let legacy_msg = match result {
                Ok(value) => ServerMessage::result_ok(&id, value),
                Err((_, msg)) => ServerMessage::result_error(&id, msg),
            };
            if let Ok(json) = serde_json::to_string(&legacy_msg) {
                responses.push(json);
            }
        }
        ClientMessage::Ping => {
            if let Ok(json) = serde_json::to_string(&ServerMessage::Pong) {
                responses.push(json);
            }
        }
    }

    responses
}
