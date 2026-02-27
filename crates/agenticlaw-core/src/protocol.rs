//! WebSocket protocol — OpenClaw-compatible JSON-RPC style
//!
//! Wire format:
//!
//! Client → Server (RPC request):
//!   { "id": "req-123", "method": "chat.send", "params": { "session": "main", "message": "Hello" } }
//!
//! Server → Client (RPC response):
//!   { "id": "req-123", "result": { "ok": true } }
//!   { "id": "req-123", "error": { "code": -1, "message": "not found" } }
//!
//! Server → Client (Event push, no id):
//!   { "event": "chat", "data": { "session": "main", "type": "delta", "content": "Hello..." } }
//!
//! Authentication:
//!   { "token": "secret" }  (shorthand)
//!   { "id": "1", "method": "auth", "params": { "token": "secret" } }  (RPC style)

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Client → Server: JSON-RPC style
// ---------------------------------------------------------------------------

/// RPC request from client.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Server → Client: RPC response
// ---------------------------------------------------------------------------

/// RPC response to client.
#[derive(Debug, Clone, Serialize)]
pub struct RpcResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// Successful response with a result value.
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }

    /// Shorthand for a method-not-found error.
    pub fn method_not_found(id: impl Into<String>, method: &str) -> Self {
        Self::err(id, -32601, format!("Method not found: {}", method))
    }

    /// Shorthand for an internal error.
    pub fn internal_error(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::err(id, -32603, message)
    }

    /// Shorthand for an auth error.
    pub fn auth_error(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::err(id, -32000, message)
    }
}

/// RPC error detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Server → Client: Event push
// ---------------------------------------------------------------------------

/// Server-pushed event (no id, no request correlation).
#[derive(Debug, Clone, Serialize)]
pub struct EventMessage {
    pub event: String,
    pub data: serde_json::Value,
}

impl EventMessage {
    pub fn new(event: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    /// Create a chat event.
    pub fn chat(session: &str, event_type: &str, data: serde_json::Value) -> Self {
        let mut map = serde_json::Map::new();
        map.insert(
            "session".to_string(),
            serde_json::Value::String(session.to_string()),
        );
        map.insert(
            "type".to_string(),
            serde_json::Value::String(event_type.to_string()),
        );
        for (k, v) in data.as_object().cloned().unwrap_or_default() {
            map.insert(k, v);
        }
        Self::new("chat", serde_json::Value::Object(map))
    }

    /// Chat delta event.
    pub fn chat_delta(session: &str, content: &str) -> Self {
        Self::chat(session, "delta", serde_json::json!({ "content": content }))
    }

    /// Chat thinking event.
    pub fn chat_thinking(session: &str, content: &str) -> Self {
        Self::chat(
            session,
            "thinking",
            serde_json::json!({ "content": content }),
        )
    }

    /// Chat tool_call event.
    pub fn chat_tool_call(session: &str, id: &str, name: &str) -> Self {
        Self::chat(
            session,
            "tool_call",
            serde_json::json!({ "id": id, "name": name }),
        )
    }

    /// Chat tool_call_delta event.
    pub fn chat_tool_call_delta(session: &str, id: &str, arguments: &str) -> Self {
        Self::chat(
            session,
            "tool_call_delta",
            serde_json::json!({ "id": id, "arguments": arguments }),
        )
    }

    /// Chat tool_result event.
    pub fn chat_tool_result(
        session: &str,
        id: &str,
        name: &str,
        result: &str,
        is_error: bool,
    ) -> Self {
        Self::chat(
            session,
            "tool_result",
            serde_json::json!({
                "id": id,
                "name": name,
                "content": result,
                "is_error": is_error,
            }),
        )
    }

    /// Chat done event.
    pub fn chat_done(session: &str) -> Self {
        Self::chat(session, "done", serde_json::json!({}))
    }

    /// Chat error event.
    pub fn chat_error(session: &str, message: &str) -> Self {
        Self::chat(session, "error", serde_json::json!({ "message": message }))
    }

    /// Auth result event (for shorthand auth without RPC id).
    pub fn auth_result(ok: bool, error: Option<&str>) -> Self {
        Self::new("auth", serde_json::json!({ "ok": ok, "error": error }))
    }

    /// Info event (sent on connection).
    pub fn info(version: &str, layer: Option<&str>) -> Self {
        Self::new(
            "info",
            serde_json::json!({ "version": version, "layer": layer }),
        )
    }

    /// Tool parked event.
    pub fn tool_parked(session: &str, id: &str, name: &str) -> Self {
        Self::chat(
            session,
            "tool_parked",
            serde_json::json!({ "id": id, "name": name }),
        )
    }

    /// Pong event.
    pub fn pong() -> Self {
        Self::new("pong", serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// Unified incoming message — handles both RPC and auth shorthand
// ---------------------------------------------------------------------------

/// Unified incoming message. Serde tries RPC first, then Auth shorthand.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum IncomingMessage {
    /// Full RPC request: { "id": "...", "method": "...", "params": ... }
    Rpc(RpcRequest),
    /// Auth shorthand: { "token": "..." } or { "token": null }
    Auth { token: Option<String> },
}

// ---------------------------------------------------------------------------
// Legacy protocol types — kept for backward compatibility
// ---------------------------------------------------------------------------

/// Client-to-server messages (legacy v2 protocol).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "auth")]
    Auth { token: Option<String> },

    #[serde(rename = "chat")]
    Chat {
        session: String,
        message: String,
        #[serde(default)]
        model: Option<String>,
    },

    #[serde(rename = "abort")]
    Abort { session: String },

    #[serde(rename = "call")]
    Call {
        id: String,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },

    #[serde(rename = "ping")]
    Ping,
}

/// Server-to-client messages (legacy v2 protocol).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "auth_result")]
    AuthResult { ok: bool, error: Option<String> },

    #[serde(rename = "delta")]
    Delta { session: String, content: String },

    #[serde(rename = "thinking")]
    Thinking { session: String, content: String },

    #[serde(rename = "tool_call")]
    ToolCall {
        session: String,
        id: String,
        name: String,
    },

    #[serde(rename = "tool_call_delta")]
    ToolCallDelta {
        session: String,
        id: String,
        arguments: String,
    },

    #[serde(rename = "done")]
    Done { session: String },

    #[serde(rename = "error")]
    Error { session: String, message: String },

    #[serde(rename = "result")]
    Result {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    #[serde(rename = "pong")]
    Pong,

    #[serde(rename = "info")]
    Info {
        version: String,
        layer: Option<String>,
    },
}

impl ServerMessage {
    pub fn auth_ok() -> Self {
        Self::AuthResult {
            ok: true,
            error: None,
        }
    }

    pub fn auth_failed(reason: impl Into<String>) -> Self {
        Self::AuthResult {
            ok: false,
            error: Some(reason.into()),
        }
    }

    pub fn delta(session: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Delta {
            session: session.into(),
            content: content.into(),
        }
    }

    pub fn done(session: impl Into<String>) -> Self {
        Self::Done {
            session: session.into(),
        }
    }

    pub fn error(session: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            session: session.into(),
            message: message.into(),
        }
    }

    pub fn result_ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self::Result {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn result_error(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self::Result {
            id: id.into(),
            result: None,
            error: Some(error.into()),
        }
    }
}
