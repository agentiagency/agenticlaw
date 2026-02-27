use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use tokio::fs;

use super::types::SessionState;

pub async fn write_in_process(
    base: &str,
    sessions: &HashMap<String, SessionState>,
) -> Result<(), String> {
    let dir = Path::new(base).join("supervisor");
    fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("mkdir: {e}"))?;

    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let mut md = format!("# In-Process Sessions\n\nUpdated: {now}\n\n");
    md.push_str("| Session | Status | Card | Context% | Unchanged | Frontier |\n");
    md.push_str("|---------|--------|------|----------|-----------|----------|\n");

    let mut names: Vec<&String> = sessions.keys().collect();
    names.sort();

    for name in names {
        let s = &sessions[name];
        let card = s.card.as_deref().unwrap_or("-");
        let ctx = s
            .context_pct
            .map(|p| format!("{p}%"))
            .unwrap_or_else(|| "-".into());
        let frontier = s.frontier_summary.as_deref().unwrap_or("-");
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            s.name, s.status, card, ctx, s.consecutive_unchanged, frontier
        ));
    }

    let path = dir.join("in-process.md");
    fs::write(&path, md)
        .await
        .map_err(|e| format!("write in-process.md: {e}"))?;

    Ok(())
}

pub async fn read_card_map(base: &str) -> HashMap<String, String> {
    let path = Path::new(base).join("supervisor").join("card-map.json");
    match fs::read_to_string(&path).await {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

pub async fn write_card_map(base: &str, map: &HashMap<String, String>) -> Result<(), String> {
    let dir = Path::new(base).join("supervisor");
    fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("mkdir: {e}"))?;

    let json = serde_json::to_string_pretty(map).map_err(|e| format!("json: {e}"))?;
    let path = dir.join("card-map.json");
    fs::write(&path, json)
        .await
        .map_err(|e| format!("write card-map: {e}"))?;

    Ok(())
}

pub fn extract_context_pct(pane: &str) -> Option<u8> {
    // Look for patterns like "45%" or "context: 45%" in the status bar area (last few lines)
    let tail: Vec<&str> = pane.lines().rev().take(5).collect();
    for line in tail {
        // Match "NN%" where NN is 0-100
        for word in line.split_whitespace() {
            if let Some(num_str) = word.strip_suffix('%') {
                if let Ok(n) = num_str.parse::<u8>() {
                    if n <= 100 {
                        return Some(n);
                    }
                }
            }
        }
    }
    None
}

pub fn extract_frontier_keywords(pane: &str) -> Option<String> {
    // Take last 20 non-empty lines as a rough summary source
    let lines: Vec<&str> = pane.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.is_empty() {
        return None;
    }

    let tail = &lines[lines.len().saturating_sub(10)..];
    let summary: String = tail.join(" ");

    // Truncate to ~50 tokens (~250 chars), respecting char boundaries
    if summary.len() > 250 {
        let boundary = summary
            .char_indices()
            .take_while(|(i, _)| *i <= 250)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        Some(format!("{}...", &summary[..boundary]))
    } else {
        Some(summary)
    }
}

/// Extract shell commands from pane content by looking for prompt-prefixed lines.
/// Returns the last N commands found (most recent last).
pub fn extract_recent_commands(pane: &str, max: usize) -> Vec<String> {
    let prompt_chars = ['$', '>', '%'];
    let mut commands = Vec::new();

    for line in pane.lines() {
        let trimmed = line.trim();
        // Match lines like "user@host:~$ command" or "$ command" or "> command"
        for &pc in &prompt_chars {
            if let Some(pos) = trimmed.find(pc) {
                let after = trimmed[pos + 1..].trim();
                if !after.is_empty() && pos < 80 {
                    // prompt prefix shouldn't be too long
                    commands.push(after.to_string());
                    break;
                }
            }
        }
    }

    if commands.len() > max {
        commands.split_off(commands.len() - max)
    } else {
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_context_pct_finds_percentage() {
        let pane = "some output\nmore output\ncontext: 67% used\nprompt>";
        assert_eq!(extract_context_pct(pane), Some(67));
    }

    #[test]
    fn extract_context_pct_none_when_missing() {
        let pane = "hello world\nno percentage here";
        assert_eq!(extract_context_pct(pane), None);
    }

    #[test]
    fn extract_frontier_keywords_truncates() {
        let long = "word ".repeat(200);
        let result = extract_frontier_keywords(&long).unwrap();
        assert!(result.len() <= 254); // 250 + "..."
    }

    #[test]
    fn extract_recent_commands_from_prompts() {
        let pane = "user@host:~$ cargo build\ncompiling...\nuser@host:~$ cargo test\nok";
        let cmds = extract_recent_commands(pane, 10);
        assert_eq!(cmds, vec!["cargo build", "cargo test"]);
    }

    #[test]
    fn extract_recent_commands_limits() {
        let pane = "$ a\n$ b\n$ c\n$ d\n$ e";
        let cmds = extract_recent_commands(pane, 3);
        assert_eq!(cmds, vec!["c", "d", "e"]);
    }
}
