use crate::context::{self, CleanContextSession, EmitOptions};
use crate::format::{format_session, FormatOptions};
use crate::openclaw::OpenclawSession;
use crate::parser::parse_lines;
use crate::session::Session;
use crate::transform::transform;
use crate::types::Record;

fn default_opts() -> FormatOptions {
    FormatOptions::default()
}

fn parse_and_format(jsonl: &str, opts: &FormatOptions) -> String {
    let result = parse_lines(jsonl);
    assert!(result.errors.is_empty(), "Parse errors: {:?}", result.errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    let events = transform(result.records);
    format_session(&events, opts)
}

#[test]
fn parse_session_record() {
    let line = r#"{"type":"session","version":3,"id":"b96c88e2-b875-4762-a8c3-fd495a27f5bd","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/home/devkit/.openclaw/workspace"}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Session(s) => {
            assert_eq!(s.version, 3);
            assert_eq!(s.id, "b96c88e2-b875-4762-a8c3-fd495a27f5bd");
            assert_eq!(s.cwd.as_deref(), Some("/home/devkit/.openclaw/workspace"));
        }
        _ => panic!("Expected Session record"),
    }
}

#[test]
fn parse_model_change() {
    let line = r#"{"type":"model_change","id":"9670fc30","parentId":null,"timestamp":"2026-02-07T17:56:16.780Z","provider":"anthropic","modelId":"claude-opus-4-6"}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::ModelChange(m) => {
            assert_eq!(m.model_id, "claude-opus-4-6");
            assert_eq!(m.provider, "anthropic");
        }
        _ => panic!("Expected ModelChange record"),
    }
}

#[test]
fn parse_thinking_level_change() {
    let line = r#"{"type":"thinking_level_change","id":"6f98e63e","parentId":"9670fc30","timestamp":"2026-02-07T17:56:16.780Z","thinkingLevel":"low"}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::ThinkingLevelChange(t) => {
            assert_eq!(t.thinking_level, "low");
        }
        _ => panic!("Expected ThinkingLevelChange record"),
    }
}

#[test]
fn parse_user_message() {
    let line = r#"{"type":"message","id":"98a691e9","parentId":"03e74840","timestamp":"2026-02-07T17:56:16.785Z","message":{"role":"user","content":[{"type":"text","text":"Hello world"}],"timestamp":1770486976784}}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Message(m) => {
            assert_eq!(m.message.role, "user");
        }
        _ => panic!("Expected Message record"),
    }
}

#[test]
fn parse_assistant_with_tool_call() {
    let line = r#"{"type":"message","id":"c47a5ac0","parentId":"98a691e9","timestamp":"2026-02-07T17:56:19.489Z","message":{"role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"thinking","thinking":"Let me read PROMPT_1.","thinkingSignature":"sig"},{"type":"toolCall","id":"toolu_01V2","name":"read","arguments":{"path":"/tmp/test.md"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-opus-4-6","usage":{"input":3,"output":91,"cacheRead":15396,"cacheWrite":434,"totalTokens":15924},"timestamp":1770486979489}}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Message(m) => {
            assert_eq!(m.message.role, "assistant");
            assert!(m.message.usage.is_some());
        }
        _ => panic!("Expected Message record"),
    }
}

#[test]
fn parse_tool_result() {
    let line = r#"{"type":"message","id":"500e1479","parentId":"c47a5ac0","timestamp":"2026-02-07T17:56:19.504Z","message":{"role":"toolResult","toolCallId":"toolu_01V2","toolName":"read","content":[{"type":"text","text":"file contents here"}],"isError":false,"timestamp":1770486979503}}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Message(m) => {
            assert_eq!(m.message.role, "toolResult");
            assert_eq!(m.message.tool_call_id.as_deref(), Some("toolu_01V2"));
        }
        _ => panic!("Expected Message record"),
    }
}

#[test]
fn parse_compaction() {
    let line = r#"{"type":"compaction","id":"4d274c64","parentId":"d4fb6552","timestamp":"2026-02-07T19:45:15.798Z","summary":"Goal: Execute PROMPT_1"}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Compaction(c) => {
            assert!(c.summary.contains("PROMPT_1"));
        }
        _ => panic!("Expected Compaction record"),
    }
}

#[test]
fn parse_custom() {
    let line = r#"{"type":"custom","customType":"model-snapshot","data":{"provider":"anthropic"},"id":"40821ce7","parentId":"6f98e63e","timestamp":"2026-02-07T17:56:16.782Z"}"#;
    let record: Record = serde_json::from_str(line).unwrap();
    match record {
        Record::Custom(c) => {
            assert_eq!(c.custom_type, "model-snapshot");
        }
        _ => panic!("Expected Custom record"),
    }
}

#[test]
fn tool_call_linkage() {
    let jsonl = r#"{"type":"session","version":3,"id":"test-session","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"message","id":"m1","parentId":"test-session","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"tc1","name":"read","arguments":{"path":"/tmp/foo.txt"}}],"timestamp":1770486977000}}
{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-02-07T17:56:17.100Z","message":{"role":"toolResult","toolCallId":"tc1","toolName":"read","content":[{"type":"text","text":"hello world"}],"isError":false,"timestamp":1770486977100}}"#;

    let output = parse_and_format(jsonl, &default_opts());
    assert!(output.contains("[tool:read] /tmp/foo.txt"));
    assert!(output.contains("hello world"));
    // toolResult should NOT appear as its own turn
    assert!(!output.contains("[toolResult]"));
}

#[test]
fn thinking_excluded_by_default() {
    let jsonl = r#"{"type":"session","version":3,"id":"test","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"message","id":"m1","parentId":"test","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"deep thoughts"},{"type":"text","text":"Hello"}],"timestamp":1770486977000}}"#;

    let output = parse_and_format(jsonl, &default_opts());
    assert!(!output.contains("deep thoughts"));
    assert!(output.contains("Hello"));
}

#[test]
fn thinking_included_with_flag() {
    let jsonl = r#"{"type":"session","version":3,"id":"test","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"message","id":"m1","parentId":"test","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"deep thoughts"},{"type":"text","text":"Hello"}],"timestamp":1770486977000}}"#;

    let mut opts = default_opts();
    opts.include_thinking = true;
    let output = parse_and_format(jsonl, &opts);
    assert!(output.contains("[thinking] deep thoughts"));
}

#[test]
fn session_header_format() {
    let jsonl = r#"{"type":"session","version":3,"id":"b96c88e2-b875-4762-a8c3-fd495a27f5bd","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/home/devkit"}"#;

    let output = parse_and_format(jsonl, &default_opts());
    assert!(output.contains("═══ Session b96c88e2 ═══"));
    assert!(output.contains("2026-02-07 17:56:16 UTC"));
    assert!(output.contains("Working directory: /home/devkit"));
}

#[test]
fn compaction_format() {
    let jsonl = r#"{"type":"session","version":3,"id":"test","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"compaction","id":"c1","parentId":"test","timestamp":"2026-02-07T19:45:15.798Z","summary":"Goal: Execute PROMPT_1\nStep 2: Configure AWS"}"#;

    let output = parse_and_format(jsonl, &default_opts());
    assert!(output.contains("[compaction]"));
    assert!(output.contains("  Goal: Execute PROMPT_1"));
}

#[test]
fn malformed_lines_produce_warnings() {
    let content = "not json at all\n{\"type\":\"session\",\"version\":3,\"id\":\"test\",\"timestamp\":\"2026-02-07T17:56:16.780Z\",\"cwd\":\"/tmp\"}";
    let result = parse_lines(content);
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].line, 1);
}

#[test]
fn empty_input() {
    let result = parse_lines("");
    assert!(result.records.is_empty());
    assert!(result.errors.is_empty());
}

#[test]
fn usage_display() {
    let jsonl = r#"{"type":"session","version":3,"id":"test","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"message","id":"m1","parentId":"test","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}],"usage":{"input":100,"output":50,"cacheRead":200,"totalTokens":350},"timestamp":1770486977000}}"#;

    let mut opts = default_opts();
    opts.include_usage = true;
    let output = parse_and_format(jsonl, &opts);
    assert!(output.contains("[tokens: 100 in, 50 out, 200 cached, 350 total]"));
}

// --- Version compat tests ---

#[test]
fn version_compat_warning_on_future_version() {
    let jsonl = r#"{"type":"session","version":99,"id":"future","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}"#;
    let session = OpenclawSession::from_str(jsonl);
    assert_eq!(session.parse_errors.len(), 1);
    assert!(session.parse_errors[0].message.contains("format version 99"));
    assert!(session.parse_errors[0].message.contains("Upgrade agenticlaw"));
}

#[test]
fn no_version_warning_on_current_version() {
    let jsonl = r#"{"type":"session","version":3,"id":"current","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}"#;
    let session = OpenclawSession::from_str(jsonl);
    assert!(session.parse_errors.is_empty());
}

// --- Session trait tests ---

#[test]
fn openclaw_session_trait_basics() {
    let jsonl = r#"{"type":"session","version":3,"id":"abc-123","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/home/devkit"}
{"type":"message","id":"m1","parentId":"abc-123","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}],"timestamp":1770486977000}}"#;

    let session = OpenclawSession::from_str(jsonl);
    assert_eq!(session.id(), "abc-123");
    assert_eq!(session.timestamp(), "2026-02-07T17:56:16.780Z");
    assert_eq!(session.cwd(), Some("/home/devkit"));
    assert!(session.parse_errors.is_empty());
    assert_eq!(session.events().len(), 2); // header + user turn
}

#[test]
fn openclaw_session_format_via_trait() {
    let jsonl = r#"{"type":"session","version":3,"id":"def-456","timestamp":"2026-02-07T17:56:16.780Z","cwd":"/tmp"}
{"type":"message","id":"m1","parentId":"def-456","timestamp":"2026-02-07T17:56:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi there"}],"timestamp":1770486977000}}"#;

    let session = OpenclawSession::from_str(jsonl);
    let output = format_session(session.events(), &default_opts());
    assert!(output.contains("═══ Session def-456 ═══"));
    assert!(output.contains("Hi there"));
}

#[test]
fn openclaw_session_parse_errors() {
    let jsonl = "not json\n{\"type\":\"session\",\"version\":3,\"id\":\"test\",\"timestamp\":\"2026-02-07T17:56:16.780Z\",\"cwd\":\"/tmp\"}";
    let session = OpenclawSession::from_str(jsonl);
    assert_eq!(session.parse_errors.len(), 1);
    assert_eq!(session.events().len(), 1);
}

// ===========================================================================
// Clean context format tests
// ===========================================================================

fn sample_jsonl() -> &'static str {
    r#"{"type":"session","version":3,"id":"ctx-test-001","timestamp":"2026-02-16T10:00:00.000Z","cwd":"/home/agent/workspace"}
{"type":"model_change","id":"mc1","parentId":null,"timestamp":"2026-02-16T10:00:00.000Z","provider":"anthropic","modelId":"claude-opus-4-6"}
{"type":"message","id":"m1","parentId":"ctx-test-001","timestamp":"2026-02-16T10:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Read the config file and tell me what port the server runs on."}],"timestamp":1770486001000}}
{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-02-16T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"I need to read the config file first."},{"type":"text","text":"Let me check the config."},{"type":"toolCall","id":"tc1","name":"read","arguments":{"file_path":"/etc/app/config.toml"}}],"usage":{"input":100,"output":50,"cacheRead":200,"totalTokens":350},"timestamp":1770486002000}}
{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-02-16T10:00:02.100Z","message":{"role":"toolResult","toolCallId":"tc1","toolName":"read","content":[{"type":"text","text":"[server]\nport = 8080\nhost = \"0.0.0.0\""}],"isError":false,"timestamp":1770486002100}}
{"type":"message","id":"m4","parentId":"m3","timestamp":"2026-02-16T10:00:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The server runs on port 8080, binding to all interfaces (0.0.0.0)."}],"timestamp":1770486003000}}
{"type":"compaction","id":"c1","parentId":"m4","timestamp":"2026-02-16T10:05:00.000Z","summary":"User asked about server config. Port is 8080 on 0.0.0.0."}"#
}

/// Helper: JSONL → SessionEvents → clean context string
fn jsonl_to_context(jsonl: &str, opts: &EmitOptions) -> String {
    let result = parse_lines(jsonl);
    let events = transform(result.records);
    context::emit(&events, opts)
}

// ---------------------------------------------------------------------------
// Emit tests
// ---------------------------------------------------------------------------

#[test]
fn context_emit_session_header() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(ctx.starts_with("--- session: ctx-test-001 ---\n"));
    assert!(ctx.contains("started: 2026-02-16T10:00:00.000Z"));
    assert!(ctx.contains("cwd: /home/agent/workspace"));
}

#[test]
fn context_emit_user_turn_has_up_tags() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(ctx.contains("<up>\n"));
    assert!(ctx.contains("Read the config file"));
    assert!(ctx.contains("</up>"));
}

#[test]
fn context_emit_assistant_turn_no_up_tags() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    // "Let me check" is assistant text — should NOT be wrapped in <up>
    let check_idx = ctx.find("Let me check").unwrap();
    let preceding = &ctx[..check_idx];
    // The last <up> before this should be closed already
    let last_up = preceding.rfind("<up>");
    let last_close = preceding.rfind("</up>");
    if let (Some(up), Some(close)) = (last_up, last_close) {
        assert!(close > up, "Assistant text should not be inside <up> tags");
    }
}

#[test]
fn context_emit_tool_interaction() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(ctx.contains("[tool:read] /etc/app/config.toml"));
    assert!(ctx.contains("  → [server]"));
    assert!(ctx.contains("port = 8080"));
}

#[test]
fn context_emit_model_change() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(ctx.contains("[model: claude-opus-4-6 (anthropic)]"));
}

#[test]
fn context_emit_compaction() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(ctx.contains("[compaction]"));
    assert!(ctx.contains("User asked about server config"));
}

#[test]
fn context_emit_thinking_excluded_by_default() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(!ctx.contains("[thinking]"));
    assert!(!ctx.contains("I need to read the config"));
}

#[test]
fn context_emit_thinking_included_with_flag() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions { include_thinking: true, ..Default::default() });
    assert!(ctx.contains("[thinking] I need to read the config file first."));
}

#[test]
fn context_emit_usage_excluded_by_default() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(!ctx.contains("[tokens:"));
}

#[test]
fn context_emit_usage_included_with_flag() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions { include_usage: true, ..Default::default() });
    assert!(ctx.contains("[tokens: 100 in, 50 out, 200 cached, 350 total]"));
}

#[test]
fn context_emit_datetime_separators() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    // Each turn should have a --- <timestamp> --- separator
    let separator_count = ctx.lines().filter(|l| l.starts_with("--- 2026-") && l.ends_with(" ---")).count();
    assert!(separator_count >= 2, "Expected at least 2 datetime separators, got {}", separator_count);
}

#[test]
fn context_emit_no_json() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    // No raw JSON should appear in clean context
    assert!(!ctx.contains(r#""type":"#));
    assert!(!ctx.contains(r#""role":"#));
}

// ---------------------------------------------------------------------------
// Parse tests (clean context → SessionEvent)
// ---------------------------------------------------------------------------

#[test]
fn context_parse_session_header() {
    let ctx = "--- session: my-session-id ---\nstarted: 2026-02-16T10:00:00.000Z\ncwd: /home/agent\n\n";
    let result = context::parse(ctx);
    assert!(result.errors.is_empty());
    assert_eq!(result.events.len(), 1);
    if let crate::transform::SessionEvent::Header { id, timestamp, cwd, .. } = &result.events[0] {
        assert_eq!(id, "my-session-id");
        assert_eq!(timestamp, "2026-02-16T10:00:00.000Z");
        assert_eq!(cwd.as_deref(), Some("/home/agent"));
    } else {
        panic!("Expected Header event");
    }
}

#[test]
fn context_parse_user_turn() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:01:00Z ---\n<up>\nHello, what is 2+2?\n</up>\n\n";
    let result = context::parse(ctx);
    assert!(result.errors.is_empty(), "Errors: {:?}", result.errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    // Header + Turn
    assert_eq!(result.events.len(), 2);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.role, "user");
        assert_eq!(turn.contents.len(), 1);
        if let crate::transform::TurnContent::Text(text) = &turn.contents[0] {
            assert_eq!(text, "Hello, what is 2+2?");
        } else {
            panic!("Expected Text content");
        }
    } else {
        panic!("Expected Turn event");
    }
}

#[test]
fn context_parse_assistant_turn() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\nThe answer is 4.\n\n";
    let result = context::parse(ctx);
    assert!(result.errors.is_empty());
    assert_eq!(result.events.len(), 2);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.role, "assistant");
        if let crate::transform::TurnContent::Text(text) = &turn.contents[0] {
            assert_eq!(text, "The answer is 4.");
        }
    } else {
        panic!("Expected Turn event");
    }
}

#[test]
fn context_parse_tool_interaction() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\n[tool:read] /tmp/foo.txt\n  → hello world\n\n";
    let result = context::parse(ctx);
    assert!(result.errors.is_empty());
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.contents.len(), 1);
        if let crate::transform::TurnContent::Tool(tool) = &turn.contents[0] {
            assert_eq!(tool.name, "read");
            assert!(tool.result.is_some());
            assert_eq!(tool.result.as_ref().unwrap().content, "hello world");
            assert!(!tool.result.as_ref().unwrap().is_error);
        } else {
            panic!("Expected Tool content");
        }
    }
}

#[test]
fn context_parse_tool_error() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\n[tool:bash] ls /nonexistent\n  → error: No such file or directory\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        if let crate::transform::TurnContent::Tool(tool) = &turn.contents[0] {
            assert!(tool.result.as_ref().unwrap().is_error);
        }
    }
}

#[test]
fn context_parse_model_change() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z [model: claude-sonnet-4-5 (anthropic)] ---\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::ModelChange { model_id, provider, .. } = &result.events[1] {
        assert_eq!(model_id, "claude-sonnet-4-5");
        assert_eq!(provider, "anthropic");
    } else {
        panic!("Expected ModelChange, got {:?}", result.events.len());
    }
}

#[test]
fn context_parse_compaction() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T01:00:00Z [compaction] ---\nUser discussed config.\nPort is 8080.\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Compaction { summary, .. } = &result.events[1] {
        assert!(summary.contains("Port is 8080"));
    } else {
        panic!("Expected Compaction event");
    }
}

#[test]
fn context_parse_thinking() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\n[thinking] I should check the file.\nThe answer is 42.\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.contents.len(), 2);
        if let crate::transform::TurnContent::Thinking(t) = &turn.contents[0] {
            assert_eq!(t, "I should check the file.");
        }
    }
}

// ---------------------------------------------------------------------------
// Round-trip tests: JSONL → clean context → parse → verify
// ---------------------------------------------------------------------------

#[test]
fn context_round_trip_preserves_structure() {
    // JSONL → events → clean context → parse → events
    let result = parse_lines(sample_jsonl());
    let original_events = transform(result.records);
    let ctx_string = context::emit(&original_events, &EmitOptions { include_thinking: true, ..Default::default() });
    let parsed = context::parse(&ctx_string);

    assert!(parsed.errors.is_empty(), "Parse errors: {:?}", parsed.errors.iter().map(|e| &e.message).collect::<Vec<_>>());

    // Count event types
    let orig_turns: Vec<_> = original_events.iter().filter(|e| matches!(e, crate::transform::SessionEvent::Turn(_))).collect();
    let parsed_turns: Vec<_> = parsed.events.iter().filter(|e| matches!(e, crate::transform::SessionEvent::Turn(_))).collect();
    assert_eq!(orig_turns.len(), parsed_turns.len(), "Turn count mismatch: {} vs {}", orig_turns.len(), parsed_turns.len());
}

#[test]
fn context_round_trip_preserves_user_role() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    for event in &parsed.events {
        if let crate::transform::SessionEvent::Turn(turn) = event {
            if turn.contents.iter().any(|c| matches!(c, crate::transform::TurnContent::Text(t) if t.contains("Read the config"))) {
                assert_eq!(turn.role, "user", "User turn not preserved as user role");
            }
        }
    }
}

#[test]
fn context_round_trip_preserves_tool_names() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let has_read_tool = parsed.events.iter().any(|e| {
        if let crate::transform::SessionEvent::Turn(turn) = e {
            turn.contents.iter().any(|c| matches!(c, crate::transform::TurnContent::Tool(t) if t.name == "read"))
        } else {
            false
        }
    });
    assert!(has_read_tool, "Tool name 'read' not preserved in round-trip");
}

#[test]
fn context_round_trip_preserves_compaction() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let has_compaction = parsed.events.iter().any(|e| matches!(e, crate::transform::SessionEvent::Compaction { .. }));
    assert!(has_compaction, "Compaction event not preserved");
}

// ---------------------------------------------------------------------------
// CleanContextSession trait tests
// ---------------------------------------------------------------------------

#[test]
fn clean_context_session_trait_basics() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx_string = context::emit(&events, &EmitOptions::default());

    let session = CleanContextSession::from_str(&ctx_string);
    assert_eq!(session.id(), "ctx-test-001");
    assert_eq!(session.timestamp(), "2026-02-16T10:00:00.000Z");
    assert_eq!(session.cwd(), Some("/home/agent/workspace"));
    assert!(session.parse_errors.is_empty());
}

#[test]
fn clean_context_session_events_count() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx_string = context::emit(&events, &EmitOptions::default());
    let session = CleanContextSession::from_str(&ctx_string);

    // header + model_change + user_turn + assistant_turn(tool) + assistant_turn(text) + compaction
    assert!(session.events().len() >= 5, "Expected at least 5 events, got {}", session.events().len());
}

#[test]
fn clean_context_session_format_via_old_formatter() {
    // Clean context → parse → format with original formatter
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let ctx_string = context::emit(&events, &EmitOptions::default());

    let session = CleanContextSession::from_str(&ctx_string);
    let output = format_session(session.events(), &FormatOptions::default());

    // The old formatter should produce readable output from clean context events
    assert!(output.contains("═══ Session ctx-test"));
    assert!(output.contains("[tool:read]"));
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn context_parse_empty() {
    let result = context::parse("");
    assert!(result.events.is_empty());
    assert!(result.errors.is_empty());
}

#[test]
fn context_parse_header_only() {
    let ctx = "--- session: lonely ---\nstarted: 2026-01-01T00:00:00Z\n\n";
    let result = context::parse(ctx);
    assert_eq!(result.events.len(), 1);
    assert!(result.errors.is_empty());
}

#[test]
fn context_parse_multiline_user_input() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:01:00Z ---\n<up>\nLine one.\nLine two.\nLine three.\n</up>\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.role, "user");
        if let crate::transform::TurnContent::Text(text) = &turn.contents[0] {
            assert!(text.contains("Line one."));
            assert!(text.contains("Line three."));
        }
    }
}

#[test]
fn context_parse_multiple_tool_calls_in_one_turn() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\nLet me check both files.\n[tool:read] /tmp/a.txt\n  → contents of a\n[tool:read] /tmp/b.txt\n  → contents of b\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        let tool_count = turn.contents.iter().filter(|c| matches!(c, crate::transform::TurnContent::Tool(_))).count();
        assert_eq!(tool_count, 2, "Expected 2 tool calls, got {}", tool_count);
    }
}

#[test]
fn context_parse_no_result_tool() {
    let ctx = "--- session: s1 ---\nstarted: 2026-01-01T00:00:00Z\n\n--- 2026-01-01T00:02:00Z ---\n[tool:write] /tmp/out.txt\n\n";
    let result = context::parse(ctx);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        if let crate::transform::TurnContent::Tool(tool) = &turn.contents[0] {
            assert!(tool.result.is_none());
        }
    }
}

#[test]
fn context_emit_is_human_readable() {
    let ctx = jsonl_to_context(sample_jsonl(), &EmitOptions::default());
    assert!(!ctx.contains(r#"\""#));
    for line in ctx.lines() {
        assert!(line.len() < 500, "Line too long for human readability: {}", &line[..80.min(line.len())]);
    }
}

// ===========================================================================
// .ctx session initialization tests
// ===========================================================================

#[test]
fn init_session_concatenates_context_files() {
    let soul = "You are an agent.\nYour purpose is to help.";
    let agents = "Available agents:\n- researcher\n- coder";
    let ctx = context::init_session(
        "test-001",
        "2026-02-16T12:00:00Z",
        Some("/workspace"),
        &[soul, agents],
    );
    assert!(ctx.starts_with("--- session: test-001 ---\n"));
    assert!(ctx.contains("cwd: /workspace"));
    assert!(ctx.contains("You are an agent."));
    assert!(ctx.contains("Available agents:"));
    // Both files in same turn block (one --- separator after header)
    let separators: Vec<_> = ctx.lines().filter(|l| l.starts_with("--- 2026-")).collect();
    assert_eq!(separators.len(), 1, "Preloaded files should be one turn, got {} separators", separators.len());
}

#[test]
fn init_session_no_context_files() {
    let ctx = context::init_session("empty-001", "2026-02-16T12:00:00Z", None, &[]);
    assert!(ctx.contains("--- session: empty-001 ---"));
    assert!(!ctx.contains("cwd:"));
    // No turn block if no context files
    let turn_seps: Vec<_> = ctx.lines().filter(|l| l.starts_with("--- 2026-")).collect();
    assert_eq!(turn_seps.len(), 0);
}

#[test]
fn init_session_parses_back() {
    let soul = "You are an agent.";
    let ctx = context::init_session("parse-001", "2026-02-16T12:00:00Z", Some("/tmp"), &[soul]);
    let result = context::parse(&ctx);
    assert!(result.errors.is_empty());
    // Header + one assistant turn (the preloaded context)
    assert_eq!(result.events.len(), 2);
    if let crate::transform::SessionEvent::Turn(turn) = &result.events[1] {
        assert_eq!(turn.role, "assistant"); // no <up>, so it's assistant
        if let crate::transform::TurnContent::Text(text) = &turn.contents[0] {
            assert!(text.contains("You are an agent."));
        }
    } else {
        panic!("Expected Turn event for preloaded context");
    }
}

// ===========================================================================
// Wire format tests (.ctx → jsonl transport)
// ===========================================================================

#[test]
fn to_wire_produces_valid_jsonl() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let wire = context::to_wire(&events);
    // Every line should be valid JSON
    for (i, line) in wire.lines().enumerate() {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "Line {} is not valid JSON: {}", i + 1, &line[..80.min(line.len())]
        );
    }
}

#[test]
fn to_wire_has_session_header() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let wire = context::to_wire(&events);
    let first: serde_json::Value = serde_json::from_str(wire.lines().next().unwrap()).unwrap();
    assert_eq!(first["type"], "session");
    assert_eq!(first["id"], "ctx-test-001");
}

#[test]
fn to_wire_preserves_roles() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let wire = context::to_wire(&events);
    let has_user = wire.lines().any(|l| l.contains(r#""role":"user""#));
    let has_assistant = wire.lines().any(|l| l.contains(r#""role":"assistant""#));
    assert!(has_user, "Wire format should contain user role");
    assert!(has_assistant, "Wire format should contain assistant role");
}

#[test]
fn to_wire_includes_tool_results() {
    let result = parse_lines(sample_jsonl());
    let events = transform(result.records);
    let wire = context::to_wire(&events);
    let has_tool_result = wire.lines().any(|l| l.contains(r#""role":"toolResult""#));
    assert!(has_tool_result, "Wire format should include tool results");
}

#[test]
fn from_wire_response_strips_jsonl() {
    let wire_line = r#"{"type":"message","id":"m1","timestamp":"2026-02-16T12:00:05Z","message":{"role":"assistant","content":[{"type":"text","text":"The answer is 42."}]}}"#;
    let ctx_fragment = context::from_wire_response(wire_line).unwrap();
    assert!(ctx_fragment.contains("--- 2026-02-16T12:00:05Z ---"));
    assert!(ctx_fragment.contains("The answer is 42."));
    assert!(!ctx_fragment.contains(r#""type""#)); // No JSON in output
}

#[test]
fn from_wire_response_ignores_non_assistant() {
    let wire_line = r#"{"type":"message","id":"m1","timestamp":"2026-02-16T12:00:05Z","message":{"role":"user","content":[{"type":"text","text":"Hello"}]}}"#;
    assert!(context::from_wire_response(wire_line).is_none());
}

// ===========================================================================
// Full lifecycle: init → append user → wire → response → append
// ===========================================================================

#[test]
fn full_ctx_lifecycle() {
    // 1. Init with SOUL.md
    let mut ctx = context::init_session(
        "lifecycle-001",
        "2026-02-16T12:00:00Z",
        Some("/workspace"),
        &["You are a helpful agent."],
    );

    // 2. User sends a message — append <up> block
    ctx.push_str("--- 2026-02-16T12:00:01Z ---\n<up>\nWhat is 2+2?\n</up>\n\n");

    // 3. Parse and convert to wire for LLM call
    let parsed = context::parse(&ctx);
    assert!(parsed.errors.is_empty());
    let wire = context::to_wire(&parsed.events);
    assert!(!wire.is_empty());

    // 4. LLM responds (simulated wire response)
    let response_wire = r#"{"type":"message","id":"r1","timestamp":"2026-02-16T12:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"2+2 equals 4."}]}}"#;
    let fragment = context::from_wire_response(response_wire).unwrap();

    // 5. Append to .ctx
    ctx.push_str(&fragment);

    // 6. Verify final .ctx parses correctly
    let final_parsed = context::parse(&ctx);
    assert!(final_parsed.errors.is_empty());
    let turns: Vec<_> = final_parsed.events.iter().filter(|e| matches!(e, crate::transform::SessionEvent::Turn(_))).collect();
    assert_eq!(turns.len(), 3, "Expected 3 turns: preload + user + assistant");
}
