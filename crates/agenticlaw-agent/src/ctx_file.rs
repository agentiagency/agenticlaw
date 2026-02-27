//! .ctx file operations — create, append, read
//!
//! A .ctx file is the source of truth for an agent session. This module
//! handles all disk I/O for .ctx files. The format is plain text:
//!
//! ```text
//! --- session: <id> ---
//! started: <ISO 8601>
//! cwd: <path>
//!
//! --- <timestamp> ---
//! [concatenated SOUL.md/AGENTS.md content]
//!
//! --- <timestamp> ---
//! <up>
//! User message here.
//! </up>
//!
//! --- <timestamp> ---
//! Assistant response here.
//! [tool:read] /path/to/file
//!
//! --- <timestamp> ---
//! <up>
//! [tool result content]
//! </up>
//! ```

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Create a new .ctx file with session header and preloaded context files.
pub fn create(
    path: &Path,
    session_id: &str,
    timestamp: &str,
    cwd: Option<&str>,
    context_files: &[String],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut out = String::new();
    out.push_str(&format!("--- session: {} ---\n", session_id));
    out.push_str(&format!("started: {}\n", timestamp));
    if let Some(cwd) = cwd {
        out.push_str(&format!("cwd: {}\n", cwd));
    }
    out.push('\n');

    if !context_files.is_empty() {
        out.push_str(&format!("--- {} ---\n", timestamp));
        for (i, content) in context_files.iter().enumerate() {
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            if i < context_files.len() - 1 {
                out.push('\n');
            }
        }
        out.push('\n');
    }

    fs::write(path, out)
}

/// Append a user message wrapped in <up> tags.
pub fn append_user_message(path: &Path, timestamp: &str, content: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).open(path)?;
    write!(f, "--- {} ---\n<up>\n{}\n</up>\n\n", timestamp, content)
}

/// Append assistant text (model output, no <up> tags).
pub fn append_assistant_text(path: &Path, timestamp: &str, content: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).open(path)?;
    write!(f, "--- {} ---\n{}\n\n", timestamp, content)
}

/// Append a tool call line to the current assistant block.
pub fn append_tool_call(path: &Path, name: &str, args_summary: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).open(path)?;
    writeln!(f, "[tool:{}] {}", name, args_summary)
}

/// Append a tool result as <up> (input to the model from outside).
pub fn append_tool_result(
    path: &Path,
    timestamp: &str,
    name: &str,
    content: &str,
    is_error: bool,
) -> std::io::Result<()> {
    let mut f = OpenOptions::new().append(true).open(path)?;
    let prefix = if is_error { "error: " } else { "" };
    // Truncate very long results for .ctx readability
    let display = if content.lines().count() > 30 {
        let lines: Vec<&str> = content.lines().collect();
        format!(
            "{}\n  ... ({} lines omitted)\n{}",
            lines[..15].join("\n"),
            lines.len() - 30,
            lines[lines.len() - 15..].join("\n")
        )
    } else {
        content.to_string()
    };
    write!(
        f,
        "--- {} ---\n<up>\n[tool:{}] {}{}\n</up>\n\n",
        timestamp,
        name,
        prefix,
        display.trim()
    )
}

/// Read the entire .ctx file contents.
pub fn read(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path)
}

/// Discover context preload files (SOUL.md, AGENTS.md, CLAUDE.md, etc.) in a workspace.
/// Also discovers KG agent identity files (EGO.md, FEAR.md, CLAUDE.md).
pub fn discover_preload_files(workspace: &Path) -> Vec<String> {
    let candidates = [
        "SOUL.md",
        "AGENTS.md",
        "USER.md",
        "TOOLS.md",
        "EGO.md",
        "FEAR.md",
        "CLAUDE.md",
    ];
    let mut contents = Vec::new();
    for name in &candidates {
        let path = workspace.join(name);
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    contents.push(content);
                }
            }
        }
    }
    contents
}

/// Resolve the sessions directory within a workspace.
/// If workspace already ends with `.agenticlaw`, use `sessions/` directly.
/// Otherwise, nest under `.agenticlaw/sessions/`.
fn sessions_dir(workspace: &Path) -> PathBuf {
    if workspace.ends_with(".agenticlaw") {
        workspace.join("sessions")
    } else {
        workspace.join(".agenticlaw").join("sessions")
    }
}

/// Generate a NEW .ctx file path for a session within a workspace.
/// Format: <workspace>/[.agenticlaw/]sessions/<YYYYMMDD-HHMMSS>-<session_id>.ctx
/// Timestamped so sessions can roll over (sleep creates a new .ctx, seeded by subconscious).
/// To resume, use `find_by_id()` which finds the latest .ctx for a given session name.
pub fn session_ctx_path(workspace: &Path, session_id: &str) -> PathBuf {
    let now = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    sessions_dir(workspace).join(format!("{}-{}.ctx", now, session_id))
}

/// Find the latest .ctx file in a workspace's session directory.
pub fn find_latest(workspace: &Path) -> Option<PathBuf> {
    let sessions_dir = sessions_dir(workspace);
    if !sessions_dir.is_dir() {
        return None;
    }

    let mut ctx_files: Vec<PathBuf> = fs::read_dir(&sessions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ctx"))
        .collect();

    // Datetime prefix means alphabetical sort = chronological sort
    ctx_files.sort();
    ctx_files.last().cloned()
}

/// Find a .ctx file by session ID. Files are named `YYYYMMDD-HHMMSS-<session_id>.ctx`,
/// so we match on the suffix. Returns the most recent match if multiple exist.
pub fn find_by_id(workspace: &Path, session_id: &str) -> Option<PathBuf> {
    let sessions_dir = sessions_dir(workspace);
    if !sessions_dir.is_dir() {
        return None;
    }

    let suffix = format!("-{}.ctx", session_id);
    let exact = format!("{}.ctx", session_id);
    let mut matches: Vec<PathBuf> = fs::read_dir(&sessions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix) || n == exact)
        })
        .collect();

    matches.sort();
    matches.last().cloned()
}

/// Parse a .ctx file back into (system_prompt, messages) for resuming a session.
/// Returns (session_id, system_prompt, Vec<(role, content)>).
pub fn parse_for_resume(path: &Path) -> std::io::Result<ResumedSession> {
    let content = fs::read_to_string(path)?;
    let mut session_id = String::new();
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<(String, String)> = Vec::new(); // (role, content)

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let mut first_turn = true;

    while i < lines.len() {
        let line = lines[i];

        // Session header
        if let Some(id) = line
            .strip_prefix("--- session: ")
            .and_then(|s| s.strip_suffix(" ---"))
        {
            session_id = id.to_string();
            i += 1;
            // Skip header fields
            while i < lines.len() && !lines[i].is_empty() {
                i += 1;
            }
            i += 1; // blank line
            continue;
        }

        // Turn separator: --- <timestamp> ---
        if line.starts_with("--- ") && line.ends_with(" ---") {
            i += 1;
            if i >= lines.len() {
                break;
            }

            // Check for <up> tag
            let is_up = lines[i] == "<up>";
            if is_up {
                i += 1;
            }

            // Collect turn content
            let mut turn_lines = Vec::new();
            while i < lines.len() {
                if is_up && lines[i] == "</up>" {
                    i += 1;
                    break;
                }
                if !is_up && lines[i].starts_with("--- ") && lines[i].ends_with(" ---") {
                    break;
                }
                if !is_up
                    && lines[i].is_empty()
                    && (i + 1 >= lines.len() || lines[i + 1].starts_with("--- "))
                {
                    i += 1;
                    break;
                }
                turn_lines.push(lines[i]);
                i += 1;
            }

            let text = turn_lines.join("\n");
            if text.trim().is_empty() {
                continue;
            }

            // First non-up turn is the preloaded system context
            if first_turn && !is_up {
                system_parts.push(text);
                first_turn = false;
                continue;
            }
            first_turn = false;

            let role = if is_up { "user" } else { "assistant" };
            messages.push((role.to_string(), text));
            continue;
        }

        i += 1;
    }

    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    Ok(ResumedSession {
        session_id,
        ctx_path: path.to_path_buf(),
        system_prompt,
        messages,
    })
}

pub struct ResumedSession {
    pub session_id: String,
    pub ctx_path: PathBuf,
    pub system_prompt: Option<String>,
    pub messages: Vec<(String, String)>,
}

/// Get current timestamp in ISO 8601 format.
pub fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn test_path() -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        temp_dir().join(format!("agenticlaw-ctx-test-{}-{}", std::process::id(), id))
    }

    #[test]
    fn create_and_read() {
        let dir = test_path();
        let path = dir.join("test.ctx");
        create(
            &path,
            "s1",
            "2026-02-16T12:00:00Z",
            Some("/workspace"),
            &[
                "You are an agent.".into(),
                "Available tools: read, write".into(),
            ],
        )
        .unwrap();

        let content = read(&path).unwrap();
        assert!(content.contains("--- session: s1 ---"));
        assert!(content.contains("cwd: /workspace"));
        assert!(content.contains("You are an agent."));
        assert!(content.contains("Available tools"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_user_and_assistant() {
        let dir = test_path();
        let path = dir.join("test.ctx");
        create(&path, "s1", "2026-02-16T12:00:00Z", None, &[]).unwrap();

        append_user_message(&path, "2026-02-16T12:00:01Z", "Hello").unwrap();
        append_assistant_text(&path, "2026-02-16T12:00:02Z", "Hi there!").unwrap();

        let content = read(&path).unwrap();
        assert!(content.contains("<up>\nHello\n</up>"));
        assert!(content.contains("Hi there!"));
        assert!(!content.contains("<up>\nHi there!")); // assistant not in <up>
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_tool_result_as_up() {
        let dir = test_path();
        let path = dir.join("test.ctx");
        create(&path, "s1", "2026-02-16T12:00:00Z", None, &[]).unwrap();

        append_tool_result(
            &path,
            "2026-02-16T12:00:03Z",
            "read",
            "file contents here",
            false,
        )
        .unwrap();

        let content = read(&path).unwrap();
        assert!(content.contains("<up>"));
        assert!(content.contains("[tool:read] file contents here"));
        assert!(content.contains("</up>"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_preload_files_finds_soul() {
        let dir = test_path();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SOUL.md"), "I am an agent.").unwrap();

        let files = discover_preload_files(&dir);
        assert_eq!(files.len(), 1);
        assert!(files[0].contains("I am an agent."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_preload_files_empty_workspace() {
        let dir = test_path();
        fs::create_dir_all(&dir).unwrap();
        let files = discover_preload_files(&dir);
        assert!(files.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_conversation_flow() {
        let dir = test_path();
        let path = dir.join("conv.ctx");

        create(
            &path,
            "conv-001",
            "2026-02-16T12:00:00Z",
            Some("/workspace"),
            &["You are helpful.".into()],
        )
        .unwrap();

        append_user_message(&path, "2026-02-16T12:00:01Z", "Read /tmp/foo.txt").unwrap();
        append_assistant_text(&path, "2026-02-16T12:00:02Z", "Let me read that file.").unwrap();
        append_tool_call(&path, "read", "/tmp/foo.txt").unwrap();
        append_tool_result(&path, "2026-02-16T12:00:03Z", "read", "hello world", false).unwrap();
        append_assistant_text(
            &path,
            "2026-02-16T12:00:04Z",
            "The file contains: hello world",
        )
        .unwrap();

        let content = read(&path).unwrap();

        // Verify structure
        assert!(content.contains("--- session: conv-001 ---"));
        assert!(content.contains("You are helpful."));
        assert!(content.contains("<up>\nRead /tmp/foo.txt\n</up>"));
        assert!(content.contains("Let me read that file."));
        assert!(content.contains("[tool:read] /tmp/foo.txt"));
        assert!(content.contains("<up>\n[tool:read] hello world\n</up>"));
        assert!(content.contains("The file contains: hello world"));

        let _ = fs::remove_dir_all(&dir);
    }
}
