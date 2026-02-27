//! Comprehensive tests for agenticlaw-core: types, protocol (v3 RPC + legacy), errors

use agenticlaw_core::*;

// ===========================================================================
// SessionKey
// ===========================================================================

#[test]
fn session_key_new_and_display() {
    let key = SessionKey::new("abc-123");
    assert_eq!(key.as_str(), "abc-123");
    assert_eq!(format!("{}", key), "abc-123");
}

#[test]
fn session_key_clone_is_cheap() {
    let key = SessionKey::new("test");
    let cloned = key.clone();
    assert_eq!(key, cloned);
    assert_eq!(key.as_str(), cloned.as_str());
}

#[test]
fn session_key_from_string() {
    let key: SessionKey = "hello".into();
    assert_eq!(key.as_str(), "hello");
    let key2: SessionKey = String::from("world").into();
    assert_eq!(key2.as_str(), "world");
}

#[test]
fn session_key_equality_and_hash() {
    use std::collections::HashSet;
    let a = SessionKey::new("same");
    let b = SessionKey::new("same");
    let c = SessionKey::new("different");
    assert_eq!(a, b);
    assert_ne!(a, c);
    let mut set = HashSet::new();
    set.insert(a.clone());
    assert!(set.contains(&b));
    assert!(!set.contains(&c));
}

// ===========================================================================
// Role
// ===========================================================================

#[test]
fn role_serde_roundtrip() {
    let roles = vec![Role::System, Role::User, Role::Assistant, Role::Tool];
    for role in roles {
        let json = serde_json::to_string(&role).unwrap();
        let back: Role = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }
}

#[test]
fn role_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), r#""user""#);
    assert_eq!(serde_json::to_string(&Role::Assistant).unwrap(), r#""assistant""#);
    assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), r#""tool""#);
}

// ===========================================================================
// Message
// ===========================================================================

#[test]
fn message_system_constructor() {
    let msg = Message::system("You are helpful");
    assert_eq!(msg.role, Role::System);
    assert_eq!(msg.content, "You are helpful");
    assert!(msg.tool_calls.is_none());
    assert!(msg.tool_call_id.is_none());
}

#[test]
fn message_user_constructor() {
    let msg = Message::user("Hello");
    assert_eq!(msg.role, Role::User);
    assert_eq!(msg.content, "Hello");
}

#[test]
fn message_assistant_constructor() {
    let msg = Message::assistant("Hi there");
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.content, "Hi there");
}

#[test]
fn message_tool_result_constructor() {
    let msg = Message::tool_result("tc-123", "file contents");
    assert_eq!(msg.role, Role::Tool);
    assert_eq!(msg.content, "file contents");
    assert_eq!(msg.tool_call_id.as_deref(), Some("tc-123"));
}

#[test]
fn message_serde_roundtrip() {
    let msg = Message::user("test message");
    let json = serde_json::to_string(&msg).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(back.role, Role::User);
    assert_eq!(back.content, "test message");
}

#[test]
fn message_tool_calls_skipped_when_none() {
    let msg = Message::user("hi");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(!json.contains("tool_calls"));
    assert!(!json.contains("tool_call_id"));
}

// ===========================================================================
// ToolCall
// ===========================================================================

#[test]
fn tool_call_serde() {
    let tc = ToolCall {
        id: "tc-1".into(),
        name: "read".into(),
        arguments: r#"{"path":"/tmp/foo"}"#.into(),
    };
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolCall = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "tc-1");
    assert_eq!(back.name, "read");
}

// ===========================================================================
// ToolDefinition
// ===========================================================================

#[test]
fn tool_definition_serde() {
    let td = ToolDefinition {
        name: "exec".into(),
        description: "Run a command".into(),
        input_schema: serde_json::json!({"type": "object"}),
    };
    let json = serde_json::to_string(&td).unwrap();
    let back: ToolDefinition = serde_json::from_str(&json).unwrap();
    assert_eq!(back.name, "exec");
    assert_eq!(back.description, "Run a command");
}

// ===========================================================================
// GatewayConfig
// ===========================================================================

#[test]
fn gateway_config_defaults() {
    let config = GatewayConfig::default();
    assert_eq!(config.port, 18789);
    assert!(matches!(config.bind, BindMode::Lan));
    assert!(matches!(config.auth.mode, AuthMode::Token));
}

#[test]
fn gateway_config_serde() {
    let config = GatewayConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    let back: GatewayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.port, 18789);
}

// ===========================================================================
// BindMode
// ===========================================================================

#[test]
fn bind_mode_to_addr() {
    assert_eq!(BindMode::Loopback.to_addr(), "127.0.0.1");
    assert_eq!(BindMode::Lan.to_addr(), "0.0.0.0");
}

// ===========================================================================
// AuthConfig / AuthMode
// ===========================================================================

#[test]
fn auth_config_defaults() {
    let config = AuthConfig::default();
    assert!(matches!(config.mode, AuthMode::Token));
    assert!(config.token.is_none());
}

// ===========================================================================
// v3 RPC Protocol — RpcRequest
// ===========================================================================

#[test]
fn rpc_request_parse_chat_send() {
    let json = r#"{"id":"req-1","method":"chat.send","params":{"session":"main","message":"hello"}}"#;
    let req: RpcRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.id, "req-1");
    assert_eq!(req.method, "chat.send");
    assert_eq!(req.params["session"], "main");
    assert_eq!(req.params["message"], "hello");
}

#[test]
fn rpc_request_parse_no_params() {
    let json = r#"{"id":"req-2","method":"sessions.list"}"#;
    let req: RpcRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.id, "req-2");
    assert_eq!(req.method, "sessions.list");
    assert!(req.params.is_null());
}

// ===========================================================================
// v3 RPC Protocol — RpcResponse
// ===========================================================================

#[test]
fn rpc_response_ok() {
    let resp = RpcResponse::ok("req-1", serde_json::json!({"ok": true}));
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains(r#""id":"req-1""#));
    assert!(json.contains(r#""ok":true"#));
    assert!(!json.contains(r#""error""#));
}

#[test]
fn rpc_response_error() {
    let resp = RpcResponse::err("req-1", -32601, "method not found");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains(r#""id":"req-1""#));
    assert!(json.contains("method not found"));
    assert!(json.contains("-32601"));
    // result should be skipped
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("result").is_none());
}

#[test]
fn rpc_response_method_not_found() {
    let resp = RpcResponse::method_not_found("req-1", "foo.bar");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("foo.bar"));
    assert!(json.contains("-32601"));
}

#[test]
fn rpc_response_internal_error() {
    let resp = RpcResponse::internal_error("req-1", "something broke");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("something broke"));
    assert!(json.contains("-32603"));
}

#[test]
fn rpc_response_auth_error() {
    let resp = RpcResponse::auth_error("req-1", "bad token");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("bad token"));
    assert!(json.contains("-32000"));
}

// ===========================================================================
// v3 RPC Protocol — EventMessage
// ===========================================================================

#[test]
fn event_message_chat_delta() {
    let evt = EventMessage::chat_delta("main", "Hello");
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""event":"chat""#));
    assert!(json.contains(r#""session":"main""#));
    assert!(json.contains(r#""type":"delta""#));
    assert!(json.contains(r#""content":"Hello""#));
}

#[test]
fn event_message_chat_done() {
    let evt = EventMessage::chat_done("main");
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""type":"done""#));
    assert!(json.contains(r#""session":"main""#));
}

#[test]
fn event_message_chat_error() {
    let evt = EventMessage::chat_error("main", "something broke");
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""type":"error""#));
    assert!(json.contains("something broke"));
}

#[test]
fn event_message_chat_tool_call() {
    let evt = EventMessage::chat_tool_call("main", "tc-1", "read");
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""type":"tool_call""#));
    assert!(json.contains(r#""id":"tc-1""#));
    assert!(json.contains(r#""name":"read""#));
}

#[test]
fn event_message_chat_tool_result() {
    let evt = EventMessage::chat_tool_result("main", "tc-1", "read", "file contents", false);
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""type":"tool_result""#));
    assert!(json.contains("file contents"));
}

#[test]
fn event_message_info() {
    let evt = EventMessage::info("0.1.0", Some("gateway"));
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""event":"info""#));
    assert!(json.contains("0.1.0"));
    assert!(json.contains("gateway"));
}

#[test]
fn event_message_pong() {
    let evt = EventMessage::pong();
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""event":"pong""#));
}

#[test]
fn event_message_auth_result() {
    let evt = EventMessage::auth_result(true, None);
    let json = serde_json::to_string(&evt).unwrap();
    assert!(json.contains(r#""event":"auth""#));
    assert!(json.contains(r#""ok":true"#));
}

// ===========================================================================
// v3 RPC Protocol — IncomingMessage
// ===========================================================================

#[test]
fn incoming_message_parses_rpc() {
    let json = r#"{"id":"req-1","method":"chat.send","params":{"session":"main","message":"hello"}}"#;
    let msg: IncomingMessage = serde_json::from_str(json).unwrap();
    match msg {
        IncomingMessage::Rpc(req) => {
            assert_eq!(req.id, "req-1");
            assert_eq!(req.method, "chat.send");
        }
        _ => panic!("Expected Rpc"),
    }
}

#[test]
fn incoming_message_parses_auth_shorthand() {
    let json = r#"{"token":"secret"}"#;
    let msg: IncomingMessage = serde_json::from_str(json).unwrap();
    match msg {
        IncomingMessage::Auth { token } => {
            assert_eq!(token.as_deref(), Some("secret"));
        }
        _ => panic!("Expected Auth"),
    }
}

#[test]
fn incoming_message_parses_auth_no_token() {
    let json = r#"{"token":null}"#;
    let msg: IncomingMessage = serde_json::from_str(json).unwrap();
    match msg {
        IncomingMessage::Auth { token } => {
            assert!(token.is_none());
        }
        _ => panic!("Expected Auth"),
    }
}

// ===========================================================================
// Legacy Protocol — ClientMessage
// ===========================================================================

#[test]
fn client_message_auth() {
    let json = r#"{"type":"auth","token":"secret"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Auth { token } => assert_eq!(token.as_deref(), Some("secret")),
        _ => panic!("Expected Auth"),
    }
}

#[test]
fn client_message_chat() {
    let json = r#"{"type":"chat","session":"s1","message":"hello","model":"claude-opus-4-6"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Chat { session, message, model } => {
            assert_eq!(session, "s1");
            assert_eq!(message, "hello");
            assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
        }
        _ => panic!("Expected Chat"),
    }
}

#[test]
fn client_message_ping() {
    let json = r#"{"type":"ping"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    assert!(matches!(msg, ClientMessage::Ping));
}

// ===========================================================================
// Legacy Protocol — ServerMessage
// ===========================================================================

#[test]
fn server_message_auth_ok() {
    let msg = ServerMessage::auth_ok();
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""ok":true"#));
    assert!(json.contains(r#""type":"auth_result""#));
}

#[test]
fn server_message_delta() {
    let msg = ServerMessage::delta("s1", "hello world");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"delta""#));
    assert!(json.contains("hello world"));
}

#[test]
fn server_message_done() {
    let msg = ServerMessage::done("s1");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"done""#));
}

#[test]
fn server_message_error() {
    let msg = ServerMessage::error("s1", "something broke");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"error""#));
    assert!(json.contains("something broke"));
}

#[test]
fn server_message_result_ok() {
    let msg = ServerMessage::result_ok("req-1", serde_json::json!({"status": "ok"}));
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"result""#));
    assert!(json.contains(r#""id":"req-1""#));
    assert!(!json.contains(r#""error""#));
}

#[test]
fn server_message_result_error() {
    let msg = ServerMessage::result_error("req-1", "not found");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("not found"));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("result").is_none(), "result field should be skipped when None");
}

#[test]
fn server_message_pong() {
    let json = serde_json::to_string(&ServerMessage::Pong).unwrap();
    assert!(json.contains(r#""type":"pong""#));
}

#[test]
fn server_message_info() {
    let msg = ServerMessage::Info { version: "0.1.0".into(), layer: Some("gateway".into()) };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"info""#));
    assert!(json.contains("0.1.0"));
    assert!(json.contains("gateway"));
}

// ===========================================================================
// Error
// ===========================================================================

#[test]
fn error_auth_failed() {
    let e = Error::auth_failed("bad creds");
    assert!(e.to_string().contains("bad creds"));
    assert!(matches!(e, Error::AuthFailed { .. }));
}

#[test]
fn error_llm_error() {
    let e = Error::llm_error("anthropic", "rate limited");
    assert!(e.to_string().contains("anthropic"));
    assert!(e.to_string().contains("rate limited"));
}

#[test]
fn error_tool_error() {
    let e = Error::tool_error("read", "file not found");
    assert!(e.to_string().contains("read"));
    assert!(e.to_string().contains("file not found"));
}

#[test]
fn error_from_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let e: Error = io_err.into();
    assert!(matches!(e, Error::IoError(_)));
}

#[test]
fn error_from_json() {
    let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let e: Error = json_err.into();
    assert!(matches!(e, Error::JsonError(_)));
}

#[test]
fn error_display_all_variants() {
    let errors: Vec<Error> = vec![
        Error::AuthFailed { reason: "x".into() },
        Error::ConnectionClosed("x".into()),
        Error::InvalidMessage("x".into()),
        Error::SessionNotFound("x".into()),
        Error::MethodNotFound("x".into()),
        Error::LlmError { provider: "p".into(), message: "m".into() },
        Error::ToolError { name: "n".into(), message: "m".into() },
        Error::ConfigError("x".into()),
        Error::Internal("x".into()),
    ];
    for e in errors {
        let _ = format!("{}", e);
    }
}
