//! Integration tests: clean context format with real session data.
//!
//! These tests load actual JSONL session fixtures, emit clean context,
//! parse it back, and verify the format works for memories, ongoing
//! context, and reference frames at all knowledge graph levels.

use agenticlaw::context::{self, CleanContextSession, EmitOptions};
use agenticlaw::format::{format_session, FormatOptions};
use agenticlaw::parser::parse_lines;
use agenticlaw::session::Session;
use agenticlaw::transform::{self, SessionEvent, TurnContent};

fn load_fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
}

fn fixture_to_events(name: &str) -> Vec<SessionEvent> {
    let content = load_fixture(name);
    let result = parse_lines(&content);
    assert!(
        result.errors.is_empty(),
        "Parse errors in {name}: {:?}",
        result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    transform::transform(result.records)
}

// ===========================================================================
// Real session data round-trip
// ===========================================================================

#[test]
fn real_session_emits_without_panic() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    assert!(!ctx.is_empty());
}

#[test]
fn real_session_parses_back_without_errors() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn real_session_preserves_session_id() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let session = CleanContextSession::from_str(&ctx);
    assert_eq!(session.id(), "real-test-2026-02-16");
}

#[test]
fn real_session_preserves_cwd() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let session = CleanContextSession::from_str(&ctx);
    assert_eq!(session.cwd(), Some("/home/devkit/agentiagency/agenticlaw"));
}

#[test]
fn real_session_preserves_turn_count() {
    let events = fixture_to_events("real-session.jsonl");
    let orig_turns: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Turn(_)))
        .collect();

    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);
    let parsed_turns: Vec<_> = parsed
        .events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Turn(_)))
        .collect();

    assert_eq!(orig_turns.len(), parsed_turns.len());
}

#[test]
fn real_session_preserves_user_messages() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let user_turns: Vec<_> = parsed
        .events
        .iter()
        .filter_map(|e| {
            if let SessionEvent::Turn(t) = e {
                if t.role == "user" { Some(t) } else { None }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(user_turns.len(), 2, "Expected 2 user turns");
    // First user message should contain the implementation request
    let first_text = user_turns[0].contents.iter().find_map(|c| {
        if let TurnContent::Text(t) = c { Some(t.as_str()) } else { None }
    });
    assert!(first_text.unwrap().contains("--watch"));
    // Second user message is "commit this"
    let second_text = user_turns[1].contents.iter().find_map(|c| {
        if let TurnContent::Text(t) = c { Some(t.as_str()) } else { None }
    });
    assert!(second_text.unwrap().contains("commit"));
}

#[test]
fn real_session_preserves_tool_interactions() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let mut tool_names: Vec<String> = Vec::new();
    for event in &parsed.events {
        if let SessionEvent::Turn(t) = event {
            for c in &t.contents {
                if let TurnContent::Tool(tool) = c {
                    tool_names.push(tool.name.clone());
                }
            }
        }
    }

    assert!(tool_names.contains(&"read".to_string()), "Missing read tool");
    assert!(tool_names.contains(&"write".to_string()), "Missing write tool");
    assert!(tool_names.contains(&"bash".to_string()), "Missing bash tool");
    assert_eq!(tool_names.len(), 5, "Expected 5 tool calls, got {}", tool_names.len());
}

#[test]
fn real_session_preserves_tool_results() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    // The read tool should have file contents
    for event in &parsed.events {
        if let SessionEvent::Turn(t) = event {
            for c in &t.contents {
                if let TurnContent::Tool(tool) = c {
                    if tool.name == "read" {
                        let result = tool.result.as_ref().expect("read tool should have result");
                        assert!(
                            result.content.contains("mod format") || result.content.contains("Parser"),
                            "read result should contain file content, got: {}",
                            &result.content[..result.content.len().min(100)]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn real_session_preserves_compaction() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let compactions: Vec<_> = parsed
        .events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Compaction { .. }))
        .collect();

    assert_eq!(compactions.len(), 1);
    if let SessionEvent::Compaction { summary, .. } = &compactions[0] {
        assert!(summary.contains("--watch"));
        assert!(summary.contains("1c36ca9"));
    }
}

#[test]
fn real_session_preserves_model_change() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let parsed = context::parse(&ctx);

    let model_changes: Vec<_> = parsed
        .events
        .iter()
        .filter_map(|e| {
            if let SessionEvent::ModelChange { model_id, provider, .. } = e {
                Some((model_id.as_str(), provider.as_str()))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(model_changes.len(), 1);
    assert_eq!(model_changes[0], ("claude-opus-4-6", "anthropic"));
}

// ===========================================================================
// Memory/context use cases
// ===========================================================================

#[test]
fn context_usable_as_memory_reference() {
    // An agent should be able to search clean context for past decisions
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());

    // Can find key decisions by text search
    assert!(ctx.contains("polling"));
    assert!(ctx.contains("HashMap"));
    assert!(ctx.contains("1c36ca9"));
    // Can find what tools were used
    assert!(ctx.contains("[tool:read]"));
    assert!(ctx.contains("[tool:bash]"));
}

#[test]
fn context_preserves_timeline() {
    // Timestamps should appear in chronological order
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());

    let timestamps: Vec<&str> = ctx
        .lines()
        .filter(|l| l.starts_with("--- 2026-") && l.ends_with(" ---"))
        .map(|l| &l[4..l.len() - 4])
        .filter(|s| !s.contains('[')) // skip annotation lines
        .collect();

    // Timestamps should be in ascending order
    for i in 1..timestamps.len() {
        assert!(
            timestamps[i] >= timestamps[i - 1],
            "Timestamps out of order: {} before {}",
            timestamps[i - 1],
            timestamps[i]
        );
    }
}

#[test]
fn context_distinguishes_input_from_output() {
    // Agents must know what was user input vs assistant output
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());

    let up_count = ctx.matches("<up>").count();
    let close_count = ctx.matches("</up>").count();
    assert_eq!(up_count, close_count, "Mismatched <up>/</ up> tags");
    assert_eq!(up_count, 2, "Expected 2 user turns");

    // User content is inside tags, assistant content is not
    let in_up = extract_up_content(&ctx);
    assert!(in_up.iter().any(|s| s.contains("--watch")));
    assert!(in_up.iter().any(|s| s.contains("commit")));

    // Assistant text should NOT be inside <up> tags
    assert!(!in_up.iter().any(|s| s.contains("Compiles with one warning")));
}

#[test]
fn context_works_with_old_formatter() {
    // Clean context → parse → old formatter (cross-format interop)
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());
    let session = CleanContextSession::from_str(&ctx);

    let formatted = format_session(session.events(), &FormatOptions::default());
    assert!(formatted.contains("═══ Session"));
    assert!(formatted.contains("[tool:read]"));
    assert!(formatted.contains("[tool:bash]"));
}

#[test]
fn context_with_thinking_provides_reasoning_chain() {
    // For orchestrator-level agents, thinking blocks are the reasoning chain
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(
        &events,
        &EmitOptions { include_thinking: true, ..Default::default() },
    );

    assert!(ctx.contains("[thinking]"));
    assert!(ctx.contains("I need to"));

    // Parse it back and verify thinking is preserved
    let parsed = context::parse(&ctx);
    let has_thinking = parsed.events.iter().any(|e| {
        if let SessionEvent::Turn(t) = e {
            t.contents.iter().any(|c| matches!(c, TurnContent::Thinking(_)))
        } else {
            false
        }
    });
    assert!(has_thinking);
}

#[test]
fn context_format_is_grep_friendly() {
    // Processes should be able to grep for tool calls, errors, user input
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());

    // Tool calls are greppable
    let tool_lines: Vec<&str> = ctx.lines().filter(|l| l.starts_with("[tool:")).collect();
    assert_eq!(tool_lines.len(), 5);

    // User input is greppable via <up> markers
    let up_lines: Vec<&str> = ctx.lines().filter(|l| *l == "<up>").collect();
    assert_eq!(up_lines.len(), 2);

    // Timestamps are greppable
    let ts_lines: Vec<&str> = ctx.lines().filter(|l| l.starts_with("--- 2026-")).collect();
    assert!(ts_lines.len() >= 5);
}

#[test]
fn context_no_json_artifacts() {
    let events = fixture_to_events("real-session.jsonl");
    let ctx = context::emit(&events, &EmitOptions::default());

    // No JSON structural characters from serialization
    assert!(!ctx.contains(r#""type":"#));
    assert!(!ctx.contains(r#""role":"#));
    assert!(!ctx.contains(r#""content":"#));
    assert!(!ctx.contains(r#""parentId":"#));
}

// ===========================================================================
// Helpers
// ===========================================================================

fn extract_up_content(ctx: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut in_up = false;
    let mut current = String::new();

    for line in ctx.lines() {
        if line == "<up>" {
            in_up = true;
            current.clear();
            continue;
        }
        if line == "</up>" {
            in_up = false;
            results.push(current.clone());
            continue;
        }
        if in_up {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    results
}
