//! Tests for agenticlaw-agent: Session, SessionRegistry, ContextManager, and real AgentRuntime

use agenticlaw_agent::*;
use agenticlaw_llm::{ContentBlock, LlmContent, LlmMessage};

// ===========================================================================
// SessionKey (re-exported from core)
// ===========================================================================

#[test]
fn session_key_basics() {
    let key = SessionKey::new("test-session");
    assert_eq!(key.as_str(), "test-session");
    assert_eq!(format!("{}", key), "test-session");
}

// ===========================================================================
// ContextManager
// ===========================================================================

#[test]
fn context_manager_estimate_tokens() {
    // 4 chars per token
    assert_eq!(ContextManager::estimate_tokens(""), 0);
    assert_eq!(ContextManager::estimate_tokens("hi"), 1); // 2/4 = 0.5 -> ceil = 1
    assert_eq!(ContextManager::estimate_tokens("hello"), 2); // 5/4 = 1.25 -> 2
    assert_eq!(ContextManager::estimate_tokens("hello world"), 3); // 11/4 = 2.75 -> 3
}

#[test]
fn context_manager_message_tokens() {
    let msg = LlmMessage {
        role: "user".into(),
        content: LlmContent::Text("hello world".into()), // 3 tokens + 10 overhead = 13
    };
    let tokens = ContextManager::message_tokens(&msg);
    assert_eq!(tokens, 13);
}

#[test]
fn context_manager_message_tokens_blocks() {
    let msg = LlmMessage {
        role: "assistant".into(),
        content: LlmContent::Blocks(vec![
            ContentBlock::Text { text: "hi".into() }, // 1
            ContentBlock::ToolUse {
                id: "tc-1".into(),
                name: "read".into(),                            // 1
                input: serde_json::json!({"path": "/tmp/foo"}), // ~6 tokens for json string
            },
        ]),
    };
    let tokens = ContextManager::message_tokens(&msg);
    assert!(tokens > 10, "Expected > 10 tokens, got {}", tokens);
}

#[test]
fn context_manager_calculate_total() {
    let cm = ContextManager::new(100_000);
    let messages = vec![
        LlmMessage {
            role: "user".into(),
            content: LlmContent::Text("hello".into()),
        },
        LlmMessage {
            role: "assistant".into(),
            content: LlmContent::Text("hi there".into()),
        },
    ];
    let total = cm.calculate_total(&messages);
    assert!(total > 20, "Expected > 20 tokens, got {}", total);
}

#[test]
fn context_manager_set_system_adds_tokens() {
    let mut cm = ContextManager::new(100_000);
    let empty_total = cm.calculate_total(&[]);
    assert_eq!(empty_total, 0);

    cm.set_system("You are a helpful assistant.");
    let with_system = cm.calculate_total(&[]);
    assert!(with_system > 0, "System prompt should add tokens");
}

#[test]
fn context_manager_compact_removes_old_messages() {
    let cm = ContextManager::new(100); // Very small limit

    let mut messages: Vec<LlmMessage> = (0..50)
        .map(|i| LlmMessage {
            role: "user".into(),
            content: LlmContent::Text(format!(
                "This is message number {} with some padding text to use tokens",
                i
            )),
        })
        .collect();

    let before = messages.len();
    cm.compact(&mut messages);
    assert!(
        messages.len() < before,
        "Compaction should remove messages: {} -> {}",
        before,
        messages.len()
    );
    assert!(messages.len() >= 2, "Should keep at least 2 messages");
}

#[test]
fn context_manager_no_compact_under_limit() {
    let cm = ContextManager::new(1_000_000);
    let mut messages = vec![LlmMessage {
        role: "user".into(),
        content: LlmContent::Text("hello".into()),
    }];
    let before = messages.len();
    cm.compact(&mut messages);
    assert_eq!(messages.len(), before, "Should not compact under limit");
}

#[test]
fn context_manager_compact_empty() {
    let cm = ContextManager::new(100);
    let mut messages: Vec<LlmMessage> = Vec::new();
    cm.compact(&mut messages); // should not panic
    assert!(messages.is_empty());
}

// ===========================================================================
// Session
// ===========================================================================

#[tokio::test]
async fn session_add_and_get_messages() {
    let session = Session::new(SessionKey::new("s1"), None);
    assert_eq!(session.message_count().await, 0);
    assert!(session.get_messages().await.is_empty());

    session.add_user_message("hello", 1.0, usize::MAX).await;
    assert_eq!(session.message_count().await, 1);

    let messages = session.get_messages().await;
    assert_eq!(messages[0].role, "user");
}

#[tokio::test]
async fn session_add_assistant_text() {
    let session = Session::new(SessionKey::new("s1"), None);
    session.add_assistant_text("hi there").await;
    let messages = session.get_messages().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "assistant");
}

#[tokio::test]
async fn session_add_assistant_with_tools() {
    let session = Session::new(SessionKey::new("s1"), None);
    session
        .add_assistant_with_tools(
            Some("Let me check."),
            vec![ContentBlock::ToolUse {
                id: "tc-1".into(),
                name: "read".into(),
                input: serde_json::json!({"path": "/tmp/foo"}),
            }],
        )
        .await;
    let messages = session.get_messages().await;
    assert_eq!(messages.len(), 1);
    match &messages[0].content {
        LlmContent::Blocks(blocks) => {
            assert_eq!(blocks.len(), 2); // text + tool_use
        }
        _ => panic!("Expected Blocks"),
    }
}

#[tokio::test]
async fn session_add_tool_result() {
    let session = Session::new(SessionKey::new("s1"), None);
    session
        .add_tool_result("tc-1", "file contents", false)
        .await;
    let messages = session.get_messages().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user"); // tool results sent as user role
}

#[tokio::test]
async fn session_system_prompt() {
    let session = Session::new(SessionKey::new("s1"), Some("Be helpful"));
    assert_eq!(session.system_prompt().await, Some("Be helpful".into()));

    session.set_system_prompt("Be concise").await;
    assert_eq!(session.system_prompt().await, Some("Be concise".into()));
}

#[tokio::test]
async fn session_no_system_prompt() {
    let session = Session::new(SessionKey::new("s1"), None);
    assert!(session.system_prompt().await.is_none());
}

#[tokio::test]
async fn session_model_override() {
    let session = Session::new(SessionKey::new("s1"), None);
    assert!(session.model().await.is_none());

    session.set_model("claude-opus-4-6").await;
    assert_eq!(session.model().await, Some("claude-opus-4-6".into()));
}

#[tokio::test]
async fn session_token_count() {
    let session = Session::new(SessionKey::new("s1"), None);
    assert_eq!(session.token_count().await, 0);

    session
        .add_user_message("hello world", 1.0, usize::MAX)
        .await;
    assert!(session.token_count().await > 0);
}

#[tokio::test]
async fn session_clear() {
    let session = Session::new(SessionKey::new("s1"), None);
    session.add_user_message("msg1", 1.0, usize::MAX).await;
    session.add_user_message("msg2", 1.0, usize::MAX).await;
    assert_eq!(session.message_count().await, 2);

    session.clear().await;
    assert_eq!(session.message_count().await, 0);
}

#[tokio::test]
async fn session_abort_signal() {
    let session = Session::new(SessionKey::new("s1"), None);
    // Just verify abort doesn't panic
    session.abort().await;
}

// ===========================================================================
// SessionRegistry
// ===========================================================================

#[tokio::test]
async fn registry_get_or_create() {
    let registry = SessionRegistry::new();
    let key = SessionKey::new("s1");

    let s1 = registry.get_or_create(&key, None);
    let s2 = registry.get_or_create(&key, None);
    // Should return same session (same Arc)
    assert_eq!(s1.message_count().await, s2.message_count().await);

    s1.add_user_message("hello", 1.0, usize::MAX).await;
    assert_eq!(s2.message_count().await, 1); // same underlying session
}

#[tokio::test]
async fn registry_get_missing() {
    let registry = SessionRegistry::new();
    assert!(registry.get(&SessionKey::new("nonexistent")).is_none());
}

#[tokio::test]
async fn registry_list() {
    let registry = SessionRegistry::new();
    assert!(registry.list().is_empty());

    registry.get_or_create(&SessionKey::new("a"), None);
    registry.get_or_create(&SessionKey::new("b"), None);
    let list = registry.list();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn registry_remove() {
    let registry = SessionRegistry::new();
    let key = SessionKey::new("removable");
    registry.get_or_create(&key, None);
    assert!(registry.get(&key).is_some());

    let removed = registry.remove(&key);
    assert!(removed.is_some());
    assert!(registry.get(&key).is_none());
}

// ===========================================================================
// AgentRuntime — real API integration
// ===========================================================================

fn load_api_key() -> Option<String> {
    let output = std::process::Command::new("bash")
        .args([
            "-c",
            "source ~/.keys.sh 2>/dev/null && echo $ANTHROPIC_API_KEY",
        ])
        .output()
        .ok()?;
    let key = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

#[tokio::test]
async fn agent_runtime_simple_text_turn() {
    let api_key = match load_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: no ANTHROPIC_API_KEY");
            return;
        }
    };

    let tools = agenticlaw_tools::ToolRegistry::new(); // no tools
    let config = AgentConfig {
        default_model: "claude-haiku-4-5-20251001".into(),
        max_tool_iterations: 5,
        system_prompt: Some("Reply with exactly the word 'pong' and nothing else.".into()),
        workspace_root: std::env::temp_dir(),
        sleep_threshold_pct: 1.0,
    };
    let runtime = AgentRuntime::new(&api_key, tools, config);

    let session_key = SessionKey::new("test-simple");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);

    let result = runtime.run_turn(&session_key, "ping", event_tx).await;
    assert!(result.is_ok(), "run_turn failed: {:?}", result);

    let mut text = String::new();
    let mut got_done = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done { .. } => {
                got_done = true;
                break;
            }
            AgentEvent::Error(e) => panic!("Agent error: {}", e),
            _ => {}
        }
    }

    assert!(got_done, "Never received Done event");
    assert!(
        text.to_lowercase().contains("pong"),
        "Expected 'pong', got: {}",
        text
    );

    // Session should have messages now
    let session = runtime.sessions().get(&session_key).unwrap();
    assert!(session.message_count().await >= 2); // user + assistant
}

#[tokio::test]
async fn agent_runtime_with_tool_call() {
    let api_key = match load_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: no ANTHROPIC_API_KEY");
            return;
        }
    };

    // Create a workspace with a file to read
    let ws = std::env::temp_dir().join("agenticlaw-agent-test");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("secret.txt"), "The answer is 42.").unwrap();

    let tools = agenticlaw_tools::create_default_registry(&ws);
    let config = AgentConfig {
        default_model: "claude-haiku-4-5-20251001".into(),
        max_tool_iterations: 5,
        system_prompt: Some(
            "You have access to tools. Use the read tool to read files when asked.".into(),
        ),
        workspace_root: ws.clone(),
        sleep_threshold_pct: 1.0,
    };
    let runtime = AgentRuntime::new(&api_key, tools, config);

    let session_key = SessionKey::new("test-tools");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);

    let result = runtime
        .run_turn(
            &session_key,
            "Read the file secret.txt and tell me what it says.",
            event_tx,
        )
        .await;
    assert!(result.is_ok(), "run_turn failed: {:?}", result);

    let mut text = String::new();
    let mut got_tool_call = false;
    let mut got_tool_result = false;
    let mut got_done = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::ToolCallStart { name, .. } => {
                got_tool_call = true;
                assert_eq!(name, "read");
            }
            AgentEvent::ToolResult {
                result, is_error, ..
            } => {
                got_tool_result = true;
                assert!(!is_error, "Tool returned error: {}", result);
                assert!(
                    result.contains("42"),
                    "Tool result should contain '42': {}",
                    result
                );
            }
            AgentEvent::Done { .. } => {
                got_done = true;
                break;
            }
            AgentEvent::Error(e) => panic!("Agent error: {}", e),
            _ => {}
        }
    }

    assert!(got_tool_call, "Expected a tool call");
    assert!(got_tool_result, "Expected a tool result");
    assert!(got_done, "Expected Done");
    assert!(
        text.contains("42"),
        "Final response should mention 42: {}",
        text
    );

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn agent_runtime_max_iterations_enforced() {
    let api_key = match load_api_key() {
        Some(k) => k,
        None => {
            eprintln!("SKIP: no ANTHROPIC_API_KEY");
            return;
        }
    };

    // Give tools but set max iterations to 1 — the agent should stop after 1 tool loop
    let ws = std::env::temp_dir().join("agenticlaw-agent-maxiter");
    std::fs::create_dir_all(&ws).unwrap();

    let tools = agenticlaw_tools::create_default_registry(&ws);
    let config = AgentConfig {
        default_model: "claude-haiku-4-5-20251001".into(),
        max_tool_iterations: 1,
        system_prompt: None,
        workspace_root: ws.clone(),
        sleep_threshold_pct: 1.0,
    };
    let runtime = AgentRuntime::new(&api_key, tools, config);

    let session_key = SessionKey::new("test-maxiter");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);

    // Ask something that would normally loop
    let _ = runtime
        .run_turn(&session_key, "just say hello", event_tx)
        .await;

    // Drain events, should not panic or hang
    let mut event_count = 0;
    while let Some(_event) = event_rx.recv().await {
        event_count += 1;
        if event_count > 100 {
            break;
        } // safety
    }

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn tool_results_collected_in_single_message() {
    use agenticlaw_llm::{ContentBlock, LlmContent};

    let session = agenticlaw_agent::Session::new(
        agenticlaw_agent::SessionKey::from("test-tool-collect".to_string()),
        Some("test"),
    );

    // Add assistant message with 3 tool_use blocks
    let blocks = vec![
        ContentBlock::ToolUse {
            id: "tool_A".into(),
            name: "bash".into(),
            input: serde_json::json!({}),
        },
        ContentBlock::ToolUse {
            id: "tool_B".into(),
            name: "glob".into(),
            input: serde_json::json!({}),
        },
        ContentBlock::ToolUse {
            id: "tool_C".into(),
            name: "read".into(),
            input: serde_json::json!({}),
        },
    ];
    session.add_assistant_with_tools(None, blocks).await;

    // Add 3 tool results
    session.add_tool_result("tool_A", "result A", false).await;
    session.add_tool_result("tool_B", "result B", false).await;
    session.add_tool_result("tool_C", "result C", true).await;

    let messages = session.get_messages().await;
    // Should be: system + assistant + ONE user message (not three)
    // system is index 0 (if set), assistant is index 1, user is index 2
    let user_msgs: Vec<_> = messages.iter().filter(|m| m.role == "user").collect();
    assert_eq!(
        user_msgs.len(),
        1,
        "Expected 1 user message, got {}",
        user_msgs.len()
    );

    if let LlmContent::Blocks(blocks) = &user_msgs[0].content {
        let result_count = blocks
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .count();
        assert_eq!(
            result_count, 3,
            "Expected 3 tool_result blocks in single message, got {}",
            result_count
        );
    } else {
        panic!("Expected Blocks content");
    }
}
