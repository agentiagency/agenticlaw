//! RPC router — dispatches JSON-RPC method calls to handlers
//!
//! Each RPC method (chat.send, chat.history, sessions.list, etc.) is handled
//! by a dedicated async function. The router maps method names to handlers.

use agenticlaw_agent::{AgentEvent, AgentRuntime, OutputEvent, SessionKey};
use agenticlaw_core::{EventMessage, RpcResponse};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

/// Connection context passed to RPC handlers.
pub struct ConnectionContext {
    pub authenticated: bool,
    pub agent: Arc<AgentRuntime>,
    pub output_tx: broadcast::Sender<OutputEvent>,
}

/// Result type for RPC handlers.
pub type RpcResult = Result<Value, (i32, String)>;

/// Route an RPC method call to the appropriate handler.
pub async fn route_rpc(method: &str, params: Value, ctx: &ConnectionContext) -> RpcResult {
    // Auth check — most methods require authentication
    if !ctx.authenticated && method != "auth" {
        return Err((-32000, "Not authenticated".to_string()));
    }

    match method {
        "chat.send" => handle_chat_send(params, ctx).await,
        "chat.history" => handle_chat_history(params, ctx).await,
        "chat.abort" => handle_chat_abort(params, ctx).await,
        "sessions.list" => handle_sessions_list(ctx).await,
        "sessions.usage" => handle_sessions_usage(params, ctx).await,
        "sessions.delete" => handle_sessions_delete(params, ctx).await,
        "ctx.read" => handle_ctx_read(params, ctx).await,
        "health" => handle_health(ctx).await,
        "tools.list" => handle_tools_list(ctx).await,
        "echo" => Ok(params),
        _ => Err((-32601, format!("Method not found: {}", method))),
    }
}

/// Convert an RPC result to an RpcResponse.
pub fn to_response(id: &str, result: RpcResult) -> RpcResponse {
    match result {
        Ok(value) => RpcResponse::ok(id, value),
        Err((code, message)) => RpcResponse::err(id, code, message),
    }
}

// ---------------------------------------------------------------------------
// chat.send — send a message to a session
// ---------------------------------------------------------------------------

async fn handle_chat_send(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?
        .to_string();
    let message = params["message"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: message".to_string()))?
        .to_string();
    let model = params["model"].as_str().map(String::from);

    let session_key = SessionKey::new(&session);

    // Set model if provided
    if let Some(m) = model {
        if let Some(sess) = ctx.agent.sessions().get(&session_key) {
            sess.set_model(&m).await;
        }
    }

    info!(
        "chat.send: session={} message={}",
        session,
        &message[..message.len().min(50)]
    );

    // Spawn the agent turn in the background
    let agent = ctx.agent.clone();
    let output_tx = ctx.output_tx.clone();
    let session_clone = session.clone();
    let sk = session_key.clone();

    tokio::spawn(async move {
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

        // Forward AgentEvents to OutputEvents on the broadcast channel
        let fwd_output_tx = output_tx.clone();
        let fwd_session = session_clone.clone();
        let fwd_agent = agent.clone();
        let fwd_sk = sk.clone();
        let forward_task = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let output = match event {
                    AgentEvent::Text(text) => OutputEvent::Delta {
                        session: fwd_session.clone(),
                        content: text,
                    },
                    AgentEvent::Thinking(text) => OutputEvent::Thinking {
                        session: fwd_session.clone(),
                        content: text,
                    },
                    AgentEvent::ToolCallStart { id, name } => OutputEvent::ToolCall {
                        session: fwd_session.clone(),
                        id,
                        name,
                    },
                    AgentEvent::ToolCallDelta { id, arguments } => OutputEvent::ToolCallDelta {
                        session: fwd_session.clone(),
                        id,
                        arguments,
                    },
                    AgentEvent::ToolExecuting { id, name } => OutputEvent::ToolExecuting {
                        session: fwd_session.clone(),
                        id,
                        name,
                    },
                    AgentEvent::ToolResult {
                        id,
                        name,
                        result,
                        is_error,
                    } => OutputEvent::ToolResult {
                        session: fwd_session.clone(),
                        id,
                        name,
                        result,
                        is_error,
                    },
                    AgentEvent::Sleep { token_count } => OutputEvent::Sleep {
                        session: fwd_session.clone(),
                        token_count,
                    },
                    AgentEvent::Done { .. } => {
                        let _ = fwd_output_tx.send(OutputEvent::Done {
                            session: fwd_session.clone(),
                        });
                        // Emit .ctx update after turn completes
                        if let Some(sess) = fwd_agent.sessions().get(&fwd_sk) {
                            if let Some(content) = sess.read_ctx() {
                                let _ = fwd_output_tx.send(OutputEvent::CtxUpdate {
                                    session: fwd_session.clone(),
                                    content,
                                });
                            }
                        }
                        continue;
                    }
                    AgentEvent::Error(e) => OutputEvent::Error {
                        session: fwd_session.clone(),
                        message: e,
                    },
                    // New events — map to existing OutputEvent types or skip
                    AgentEvent::AgentStart
                    | AgentEvent::TurnStart { .. }
                    | AgentEvent::TurnEnd { .. }
                    | AgentEvent::ToolSkipped { .. }
                    | AgentEvent::SteeringInjected { .. }
                    | AgentEvent::FollowUpInjected { .. }
                    | AgentEvent::Aborted => continue,
                };
                let _ = fwd_output_tx.send(output);
            }
        });

        let result = agent.run_turn(&sk, &message, event_tx).await;
        let _ = forward_task.await;

        if let Err(e) = result {
            let _ = output_tx.send(OutputEvent::Error {
                session: session_clone,
                message: e,
            });
        }
    });

    // Return immediately — events stream via the broadcast channel
    Ok(serde_json::json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// chat.history — get conversation history
// ---------------------------------------------------------------------------

async fn handle_chat_history(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?;

    let session_key = SessionKey::new(session);
    let sess = ctx
        .agent
        .sessions()
        .get(&session_key)
        .ok_or_else(|| (-32001, format!("Session not found: {}", session)))?;

    let messages = sess.get_messages().await;
    let token_count = sess.token_count().await;
    let model = sess.model().await;

    // Convert LlmMessages to a JSON-friendly format
    let msg_json: Vec<Value> = messages.iter().map(|m| {
        serde_json::json!({
            "role": m.role,
            "content": match &m.content {
                agenticlaw_llm::LlmContent::Text(s) => serde_json::Value::String(s.clone()),
                agenticlaw_llm::LlmContent::Blocks(blocks) => serde_json::to_value(blocks).unwrap_or_default(),
            }
        })
    }).collect();

    Ok(serde_json::json!({
        "session": session,
        "messages": msg_json,
        "token_count": token_count,
        "model": model,
    }))
}

// ---------------------------------------------------------------------------
// chat.abort — abort the current agent turn
// ---------------------------------------------------------------------------

async fn handle_chat_abort(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?;

    let session_key = SessionKey::new(session);
    if let Some(sess) = ctx.agent.sessions().get(&session_key) {
        sess.abort().await;
        info!("Aborted session: {}", session);
        Ok(serde_json::json!({ "ok": true }))
    } else {
        Err((-32001, format!("Session not found: {}", session)))
    }
}

// ---------------------------------------------------------------------------
// sessions.list — list all sessions
// ---------------------------------------------------------------------------

async fn handle_sessions_list(ctx: &ConnectionContext) -> RpcResult {
    let sessions: Vec<String> = ctx
        .agent
        .sessions()
        .list()
        .into_iter()
        .map(|k| k.as_str().to_string())
        .collect();
    Ok(serde_json::json!({ "sessions": sessions }))
}

// ---------------------------------------------------------------------------
// sessions.usage — get token usage for a session
// ---------------------------------------------------------------------------

async fn handle_sessions_usage(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?;

    let session_key = SessionKey::new(session);
    let sess = ctx
        .agent
        .sessions()
        .get(&session_key)
        .ok_or_else(|| (-32001, format!("Session not found: {}", session)))?;

    let token_count = sess.token_count().await;
    let message_count = sess.message_count().await;
    let model = sess.model().await;

    Ok(serde_json::json!({
        "session": session,
        "token_count": token_count,
        "message_count": message_count,
        "model": model,
    }))
}

// ---------------------------------------------------------------------------
// sessions.delete — delete a session
// ---------------------------------------------------------------------------

async fn handle_sessions_delete(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?;

    let session_key = SessionKey::new(session);
    match ctx.agent.sessions().remove(&session_key) {
        Some(_) => {
            info!("Deleted session: {}", session);
            Ok(serde_json::json!({ "ok": true }))
        }
        None => Err((-32001, format!("Session not found: {}", session))),
    }
}

// ---------------------------------------------------------------------------
// health — health check
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ctx.read — return full .ctx file contents for a session
// ---------------------------------------------------------------------------

async fn handle_ctx_read(params: Value, ctx: &ConnectionContext) -> RpcResult {
    let session = params["session"]
        .as_str()
        .ok_or_else(|| (-32602, "Missing required param: session".to_string()))?;

    let session_key = SessionKey::new(session);
    let sess = ctx
        .agent
        .sessions()
        .get(&session_key)
        .ok_or_else(|| (-32002, format!("Session not found: {}", session)))?;

    match sess.read_ctx() {
        Some(content) => Ok(serde_json::json!({
            "session": session,
            "content": content,
        })),
        None => Ok(serde_json::json!({
            "session": session,
            "content": null,
        })),
    }
}

// ---------------------------------------------------------------------------
// health — gateway health check
// ---------------------------------------------------------------------------

async fn handle_health(ctx: &ConnectionContext) -> RpcResult {
    Ok(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "sessions": ctx.agent.sessions().list().len(),
        "tools": ctx.agent.tool_definitions().len(),
    }))
}

// ---------------------------------------------------------------------------
// tools.list — list available tools
// ---------------------------------------------------------------------------

async fn handle_tools_list(ctx: &ConnectionContext) -> RpcResult {
    let tools: Vec<Value> = ctx
        .agent
        .tool_definitions()
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
            })
        })
        .collect();
    Ok(serde_json::json!({ "tools": tools }))
}

/// Convert an OutputEvent to an EventMessage for WebSocket transmission.
pub fn output_event_to_message(event: &OutputEvent) -> EventMessage {
    match event {
        OutputEvent::Delta { session, content } => EventMessage::chat_delta(session, content),
        OutputEvent::Thinking { session, content } => EventMessage::chat_thinking(session, content),
        OutputEvent::ToolCall { session, id, name } => {
            EventMessage::chat_tool_call(session, id, name)
        }
        OutputEvent::ToolCallDelta {
            session,
            id,
            arguments,
        } => EventMessage::chat_tool_call_delta(session, id, arguments),
        OutputEvent::ToolExecuting { session, id, name } => EventMessage::chat(
            session,
            "tool_executing",
            serde_json::json!({ "id": id, "name": name }),
        ),
        OutputEvent::ToolResult {
            session,
            id,
            name,
            result,
            is_error,
        } => EventMessage::chat_tool_result(session, id, name, result, *is_error),
        OutputEvent::ToolParked { session, id, name } => {
            EventMessage::tool_parked(session, id, name)
        }
        OutputEvent::Done { session } => EventMessage::chat_done(session),
        OutputEvent::Error { session, message } => EventMessage::chat_error(session, message),
        OutputEvent::Sleep {
            session,
            token_count,
        } => EventMessage::chat(
            session,
            "sleep",
            serde_json::json!({ "token_count": token_count }),
        ),
        OutputEvent::CtxUpdate { session, content } => EventMessage::chat(
            session,
            "ctx_update",
            serde_json::json!({ "content": content }),
        ),
    }
}
