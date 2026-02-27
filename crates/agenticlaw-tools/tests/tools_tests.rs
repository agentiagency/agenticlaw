//! Tests for agenticlaw-tools: ToolResult, ToolRegistry, and all builtin tools against real filesystem

use agenticlaw_tools::*;
use serde_json::json;
use std::path::PathBuf;

fn test_workspace() -> PathBuf {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("agenticlaw-tools-test-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// ===========================================================================
// ToolResult
// ===========================================================================

#[test]
fn tool_result_text() {
    let r = ToolResult::text("hello");
    assert!(!r.is_error());
    assert_eq!(r.to_content_string(), "hello");
}

#[test]
fn tool_result_error() {
    let r = ToolResult::error("boom");
    assert!(r.is_error());
    assert_eq!(r.to_content_string(), "Error: boom");
}

#[test]
fn tool_result_json() {
    let r = ToolResult::Json(json!({"key": "value"}));
    assert!(!r.is_error());
    let s = r.to_content_string();
    assert!(s.contains("key"));
    assert!(s.contains("value"));
}

// ===========================================================================
// ToolRegistry
// ===========================================================================

#[tokio::test]
async fn registry_default_is_empty() {
    let reg = ToolRegistry::new();
    assert!(reg.list().is_empty());
    assert!(reg.get_definitions().is_empty());
}

#[tokio::test]
async fn registry_execute_missing_tool() {
    let reg = ToolRegistry::new();
    let result = reg.execute("nonexistent", json!({})).await;
    assert!(result.is_error());
    assert!(result.to_content_string().contains("not found"));
}

#[tokio::test]
async fn create_default_registry_has_all_tools() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let names = reg.list();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"write"));
    assert!(names.contains(&"edit"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"glob"));
    assert!(names.contains(&"grep"));
    assert_eq!(names.len(), 7);
    assert_eq!(reg.get_definitions().len(), 7);
    cleanup(&ws);
}

#[tokio::test]
async fn registry_get_tool() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    assert!(reg.get("read").is_some());
    assert!(reg.get("nonexistent").is_none());
    cleanup(&ws);
}

#[tokio::test]
async fn registry_tool_has_schema() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let defs = reg.get_definitions();
    for def in &defs {
        assert!(!def.name.is_empty());
        assert!(!def.description.is_empty());
        assert!(def.input_schema.is_object());
    }
    cleanup(&ws);
}

// ===========================================================================
// WriteTool — real filesystem
// ===========================================================================

#[tokio::test]
async fn write_tool_creates_file() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("write", json!({
        "path": "test_write.txt",
        "content": "hello world"
    })).await;
    assert!(!result.is_error(), "Write failed: {}", result.to_content_string());
    let content = std::fs::read_to_string(ws.join("test_write.txt")).unwrap();
    assert_eq!(content, "hello world");
    cleanup(&ws);
}

#[tokio::test]
async fn write_tool_creates_subdirectories() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("write", json!({
        "path": "sub/dir/deep.txt",
        "content": "nested"
    })).await;
    assert!(!result.is_error());
    assert!(ws.join("sub/dir/deep.txt").exists());
    cleanup(&ws);
}

#[tokio::test]
async fn write_tool_missing_content() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("write", json!({"path": "foo.txt"})).await;
    assert!(result.is_error());
    cleanup(&ws);
}

#[tokio::test]
async fn write_tool_missing_path() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("write", json!({"content": "stuff"})).await;
    assert!(result.is_error());
    cleanup(&ws);
}

// ===========================================================================
// ReadTool — real filesystem
// ===========================================================================

#[tokio::test]
async fn read_tool_reads_file() {
    let ws = test_workspace();
    std::fs::write(ws.join("readable.txt"), "line1\nline2\nline3").unwrap();
    let reg = create_default_registry(&ws);
    let result = reg.execute("read", json!({"path": "readable.txt"})).await;
    assert!(!result.is_error());
    let content = result.to_content_string();
    assert!(content.contains("line1"));
    assert!(content.contains("line3"));
    cleanup(&ws);
}

#[tokio::test]
async fn read_tool_with_offset_and_limit() {
    let ws = test_workspace();
    let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
    std::fs::write(ws.join("big.txt"), lines.join("\n")).unwrap();
    let reg = create_default_registry(&ws);

    let result = reg.execute("read", json!({"path": "big.txt", "offset": 10, "limit": 5})).await;
    assert!(!result.is_error());
    let content = result.to_content_string();
    assert!(content.contains("line 10"));
    assert!(content.contains("line 14"));
    assert!(!content.contains("line 9"));
    assert!(!content.contains("line 15"));
    cleanup(&ws);
}

#[tokio::test]
async fn read_tool_missing_file() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("read", json!({"path": "nonexistent.txt"})).await;
    assert!(result.is_error());
    cleanup(&ws);
}

#[tokio::test]
async fn read_tool_missing_path_param() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("read", json!({})).await;
    assert!(result.is_error());
    cleanup(&ws);
}

#[tokio::test]
async fn read_tool_path_escape_blocked() {
    let ws = test_workspace();
    // Create a file outside workspace
    let outside = std::env::temp_dir().join("agenticlaw-outside.txt");
    std::fs::write(&outside, "secret").unwrap();

    let reg = create_default_registry(&ws);
    let result = reg.execute("read", json!({"path": "../agenticlaw-outside.txt"})).await;
    assert!(result.is_error(), "Should block path escape, got: {}", result.to_content_string());
    let _ = std::fs::remove_file(outside);
    cleanup(&ws);
}

// ===========================================================================
// EditTool — real filesystem
// ===========================================================================

#[tokio::test]
async fn edit_tool_replaces_text() {
    let ws = test_workspace();
    std::fs::write(ws.join("editable.txt"), "hello world").unwrap();
    let reg = create_default_registry(&ws);
    let result = reg.execute("edit", json!({
        "path": "editable.txt",
        "old_string": "world",
        "new_string": "agenticlaw"
    })).await;
    assert!(!result.is_error());
    let content = std::fs::read_to_string(ws.join("editable.txt")).unwrap();
    assert_eq!(content, "hello agenticlaw");
    cleanup(&ws);
}

#[tokio::test]
async fn edit_tool_old_string_not_found() {
    let ws = test_workspace();
    std::fs::write(ws.join("edit2.txt"), "hello").unwrap();
    let reg = create_default_registry(&ws);
    let result = reg.execute("edit", json!({
        "path": "edit2.txt",
        "old_string": "nonexistent",
        "new_string": "replaced"
    })).await;
    assert!(result.is_error());
    assert!(result.to_content_string().contains("not found"));
    cleanup(&ws);
}

#[tokio::test]
async fn edit_tool_missing_params() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    assert!(reg.execute("edit", json!({"path": "f.txt"})).await.is_error());
    assert!(reg.execute("edit", json!({"path": "f.txt", "old_string": "x"})).await.is_error());
    cleanup(&ws);
}

// ===========================================================================
// ExecTool — real commands
// ===========================================================================

#[tokio::test]
async fn exec_tool_runs_command() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({"command": "echo hello"})).await;
    assert!(!result.is_error());
    assert_eq!(result.to_content_string(), "hello");
    cleanup(&ws);
}

#[tokio::test]
async fn exec_tool_captures_exit_code() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({"command": "exit 42"})).await;
    let content = result.to_content_string();
    assert!(content.contains("42"), "Should contain exit code 42: {}", content);
    cleanup(&ws);
}

#[tokio::test]
async fn exec_tool_captures_stderr() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({"command": "echo err >&2"})).await;
    let content = result.to_content_string();
    assert!(content.contains("err"));
    cleanup(&ws);
}

#[tokio::test]
async fn bash_tool_runs_in_workspace() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({"command": "pwd"})).await;
    assert!(result.to_content_string().contains(&ws.to_string_lossy().to_string()));
    cleanup(&ws);
}

#[tokio::test]
async fn exec_tool_timeout() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({
        "command": "sleep 60",
        "timeout": 1
    })).await;
    assert!(result.is_error());
    assert!(result.to_content_string().contains("timed out"));
    cleanup(&ws);
}

#[tokio::test]
async fn exec_tool_missing_command() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({})).await;
    assert!(result.is_error());
    cleanup(&ws);
}

#[tokio::test]
async fn exec_tool_empty_output() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);
    let result = reg.execute("bash", json!({"command": "true"})).await;
    assert!(!result.is_error());
    assert_eq!(result.to_content_string(), "(no output)");
    cleanup(&ws);
}

// ===========================================================================
// End-to-end: write then read then edit then read
// ===========================================================================

#[tokio::test]
async fn write_read_edit_read_cycle() {
    let ws = test_workspace();
    let reg = create_default_registry(&ws);

    // Write
    let r = reg.execute("write", json!({"path": "cycle.txt", "content": "alpha beta gamma"})).await;
    assert!(!r.is_error());

    // Read
    let r = reg.execute("read", json!({"path": "cycle.txt"})).await;
    assert!(r.to_content_string().contains("alpha beta gamma"));

    // Edit
    let r = reg.execute("edit", json!({"path": "cycle.txt", "old_string": "beta", "new_string": "BETA"})).await;
    assert!(!r.is_error());

    // Read again
    let r = reg.execute("read", json!({"path": "cycle.txt"})).await;
    assert!(r.to_content_string().contains("alpha BETA gamma"));

    cleanup(&ws);
}
