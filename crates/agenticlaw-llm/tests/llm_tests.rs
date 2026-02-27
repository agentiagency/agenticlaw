//! Tests for agenticlaw-llm: types, provider trait, and real Anthropic API integration

use agenticlaw_llm::*;

// ===========================================================================
// LlmRequest
// ===========================================================================

#[test]
fn llm_request_default() {
    let req = LlmRequest::default();
    assert!(req.model.contains("claude"));
    assert!(req.messages.is_empty());
    assert!(req.tools.is_none());
    assert_eq!(req.max_tokens, Some(8192));
    assert!(req.temperature.is_none());
    assert!(req.system.is_none());
}

// ===========================================================================
// LlmContent
// ===========================================================================

#[test]
fn llm_content_from_string() {
    let c: LlmContent = "hello".into();
    match c {
        LlmContent::Text(s) => assert_eq!(s, "hello"),
        _ => panic!("Expected Text"),
    }
}

#[test]
fn llm_content_from_owned_string() {
    let c: LlmContent = String::from("world").into();
    match c {
        LlmContent::Text(s) => assert_eq!(s, "world"),
        _ => panic!("Expected Text"),
    }
}

#[test]
fn llm_content_text_serde() {
    let c = LlmContent::Text("hello".into());
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(json, r#""hello""#);
    let back: LlmContent = serde_json::from_str(&json).unwrap();
    match back {
        LlmContent::Text(s) => assert_eq!(s, "hello"),
        _ => panic!("Expected Text"),
    }
}

#[test]
fn llm_content_blocks_serde() {
    let c = LlmContent::Blocks(vec![
        ContentBlock::Text { text: "hi".into() },
    ]);
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains(r#""type":"text""#));
    let back: LlmContent = serde_json::from_str(&json).unwrap();
    match back {
        LlmContent::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                ContentBlock::Text { text } => assert_eq!(text, "hi"),
                _ => panic!("Expected Text block"),
            }
        }
        _ => panic!("Expected Blocks"),
    }
}

// ===========================================================================
// ContentBlock
// ===========================================================================

#[test]
fn content_block_text_serde() {
    let b = ContentBlock::Text { text: "hello".into() };
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains(r#""type":"text""#));
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    match back {
        ContentBlock::Text { text } => assert_eq!(text, "hello"),
        _ => panic!("Expected Text"),
    }
}

#[test]
fn content_block_tool_use_serde() {
    let b = ContentBlock::ToolUse {
        id: "tc-1".into(),
        name: "read".into(),
        input: serde_json::json!({"path": "/tmp/foo"}),
    };
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains(r#""type":"tool_use""#));
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    match back {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "tc-1");
            assert_eq!(name, "read");
            assert_eq!(input["path"], "/tmp/foo");
        }
        _ => panic!("Expected ToolUse"),
    }
}

#[test]
fn content_block_tool_result_serde() {
    let b = ContentBlock::ToolResult {
        tool_use_id: "tc-1".into(),
        content: "file contents".into(),
        is_error: Some(false),
    };
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains(r#""type":"tool_result""#));
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    match back {
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
            assert_eq!(tool_use_id, "tc-1");
            assert_eq!(content, "file contents");
            assert_eq!(is_error, Some(false));
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn content_block_tool_result_no_error_skipped() {
    let b = ContentBlock::ToolResult {
        tool_use_id: "tc-1".into(),
        content: "ok".into(),
        is_error: None,
    };
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("is_error"));
}

// ===========================================================================
// LlmTool
// ===========================================================================

#[test]
fn llm_tool_serde() {
    let tool = LlmTool {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    };
    let json = serde_json::to_string(&tool).unwrap();
    let back: LlmTool = serde_json::from_str(&json).unwrap();
    assert_eq!(back.name, "read");
}

// ===========================================================================
// LlmMessage
// ===========================================================================

#[test]
fn llm_message_serde() {
    let msg = LlmMessage {
        role: "user".into(),
        content: LlmContent::Text("hello".into()),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let back: LlmMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.role, "user");
}

// ===========================================================================
// AccumulatedToolCall
// ===========================================================================

#[test]
fn accumulated_tool_call_parse_valid() {
    let tc = AccumulatedToolCall {
        id: "tc-1".into(),
        name: "read".into(),
        arguments: r#"{"path":"/tmp/foo"}"#.into(),
    };
    let parsed = tc.parse_arguments().unwrap();
    assert_eq!(parsed["path"], "/tmp/foo");
}

#[test]
fn accumulated_tool_call_parse_invalid() {
    let tc = AccumulatedToolCall {
        id: "tc-1".into(),
        name: "read".into(),
        arguments: "not json".into(),
    };
    assert!(tc.parse_arguments().is_err());
}

#[test]
fn accumulated_tool_call_default() {
    let tc = AccumulatedToolCall::default();
    assert!(tc.id.is_empty());
    assert!(tc.name.is_empty());
    assert!(tc.arguments.is_empty());
}

// ===========================================================================
// Usage
// ===========================================================================

#[test]
fn usage_default() {
    let u = Usage::default();
    assert_eq!(u.input_tokens, 0);
    assert_eq!(u.output_tokens, 0);
}

#[test]
fn usage_serde() {
    let u = Usage { input_tokens: 100, output_tokens: 50 };
    let json = serde_json::to_string(&u).unwrap();
    let back: Usage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.input_tokens, 100);
    assert_eq!(back.output_tokens, 50);
}

// ===========================================================================
// AnthropicProvider — real API integration
// ===========================================================================

fn load_api_key() -> Option<String> {
    let output = std::process::Command::new("bash")
        .args(["-c", "source ~/.keys.sh 2>/dev/null && echo $ANTHROPIC_API_KEY"])
        .output()
        .ok()?;
    let key = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if key.is_empty() { None } else { Some(key) }
}

#[tokio::test]
async fn anthropic_provider_simple_text_response() {
    let api_key = match load_api_key() {
        Some(k) => k,
        None => { eprintln!("SKIP: no ANTHROPIC_API_KEY"); return; }
    };

    let provider = AnthropicProvider::new(&api_key);
    assert_eq!(provider.name(), "anthropic");
    assert!(provider.models().len() > 0);

    let request = LlmRequest {
        model: "claude-haiku-4-5-20251001".into(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: LlmContent::Text("Reply with exactly the word 'pong' and nothing else.".into()),
        }],
        max_tokens: Some(32),
        ..Default::default()
    };

    use futures::StreamExt;
    let stream = provider.complete_stream(request).await.expect("API call failed");
    tokio::pin!(stream);

    let mut text = String::new();
    let mut got_done = false;

    while let Some(result) = stream.next().await {
        match result.expect("Stream error") {
            StreamDelta::Text(t) => text.push_str(&t),
            StreamDelta::Done { .. } => got_done = true,
            _ => {}
        }
    }

    let lower = text.to_lowercase();
    assert!(lower.contains("pong"), "Expected 'pong' in response, got: {}", text);
    assert!(got_done, "Never received Done delta");
}

#[tokio::test]
async fn anthropic_provider_with_tools() {
    let api_key = match load_api_key() {
        Some(k) => k,
        None => { eprintln!("SKIP: no ANTHROPIC_API_KEY"); return; }
    };

    let provider = AnthropicProvider::new(&api_key);

    let request = LlmRequest {
        model: "claude-haiku-4-5-20251001".into(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: LlmContent::Text("Use the get_weather tool for Paris.".into()),
        }],
        tools: Some(vec![LlmTool {
            name: "get_weather".into(),
            description: "Get weather for a city".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"}
                },
                "required": ["city"]
            }),
        }]),
        max_tokens: Some(256),
        ..Default::default()
    };

    use futures::StreamExt;
    let stream = provider.complete_stream(request).await.expect("API call failed");
    tokio::pin!(stream);

    let mut got_tool_start = false;
    let mut tool_name = String::new();
    let mut tool_args = String::new();
    let mut got_tool_end = false;

    while let Some(result) = stream.next().await {
        match result.expect("Stream error") {
            StreamDelta::ToolCallStart { name, .. } => {
                got_tool_start = true;
                tool_name = name;
            }
            StreamDelta::ToolCallDelta { arguments, .. } => {
                tool_args.push_str(&arguments);
            }
            StreamDelta::ToolCallEnd { .. } => {
                got_tool_end = true;
            }
            _ => {}
        }
    }

    assert!(got_tool_start, "Expected tool call start");
    assert_eq!(tool_name, "get_weather");
    assert!(got_tool_end, "Expected tool call end");

    // Parse accumulated arguments
    let args: serde_json::Value = serde_json::from_str(&tool_args)
        .unwrap_or_else(|e| panic!("Failed to parse tool args '{}': {}", tool_args, e));
    let city = args["city"].as_str().unwrap_or("");
    assert!(city.to_lowercase().contains("paris"), "Expected Paris in args, got: {}", city);
}

#[tokio::test]
async fn anthropic_provider_bad_key_fails() {
    let provider = AnthropicProvider::new("sk-bad-key-12345");

    let request = LlmRequest {
        model: "claude-haiku-4-5-20251001".into(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: LlmContent::Text("hello".into()),
        }],
        max_tokens: Some(16),
        ..Default::default()
    };

    let result = provider.complete_stream(request).await;
    assert!(result.is_err(), "Expected error with bad API key");
}

#[test]
fn anthropic_provider_supports_model() {
    let provider = AnthropicProvider::new("fake");
    assert!(provider.supports_model("claude-haiku-4-5-20251001"));
    assert!(!provider.supports_model("gpt-4"));
}
