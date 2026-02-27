use crate::transform::*;

pub struct FormatOptions {
    pub include_thinking: bool,
    pub include_usage: bool,
    pub summary_only: bool,
    pub raw: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_usage: false,
            summary_only: false,
            raw: false,
        }
    }
}

const MAX_TOOL_RESULT_LINES: usize = 20;

pub fn format_session(events: &[SessionEvent], opts: &FormatOptions) -> String {
    let mut out = String::new();

    for event in events {
        match event {
            SessionEvent::Header {
                id, timestamp, cwd, ..
            } => {
                let short_id = if id.len() > 8 { &id[..8] } else { id };
                out.push_str(&format!("═══ Session {} ═══\n", short_id));
                out.push_str(&format!("Started: {}\n", format_timestamp(timestamp)));
                if let Some(cwd) = cwd {
                    out.push_str(&format!("Working directory: {}\n", cwd));
                }
                out.push('\n');
            }
            SessionEvent::ModelChange {
                timestamp,
                provider,
                model_id,
            } => {
                out.push_str(&format!(
                    "  [model: {} ({}) at {}]\n\n",
                    model_id,
                    provider,
                    format_timestamp(timestamp)
                ));
            }
            SessionEvent::ThinkingLevelChange { timestamp, level } => {
                out.push_str(&format!(
                    "  [thinking: {} at {}]\n\n",
                    level,
                    format_timestamp(timestamp)
                ));
            }
            SessionEvent::Turn(turn) => {
                if opts.summary_only {
                    continue;
                }
                out.push_str(&format!(
                    "─── {} [{}] ───\n",
                    format_timestamp(&turn.timestamp),
                    turn.role
                ));

                for content in &turn.contents {
                    match content {
                        TurnContent::Text(text) => {
                            out.push_str(text);
                            out.push('\n');
                        }
                        TurnContent::Thinking(thinking) => {
                            if opts.include_thinking {
                                out.push_str("[thinking] ");
                                out.push_str(thinking);
                                out.push('\n');
                            }
                        }
                        TurnContent::Tool(interaction) => {
                            format_tool_interaction(&mut out, interaction, opts);
                        }
                    }
                }

                if opts.include_usage {
                    if let Some(ref usage) = turn.usage {
                        let input = usage.input.unwrap_or(0);
                        let output = usage.output.unwrap_or(0);
                        let cache = usage.cache_read.unwrap_or(0);
                        let total = usage.total_tokens.unwrap_or(0);
                        out.push_str(&format!(
                            "  [tokens: {} in, {} out, {} cached, {} total]\n",
                            input, output, cache, total
                        ));
                    }
                }

                out.push('\n');
            }
            SessionEvent::Compaction { timestamp, summary } => {
                out.push_str(&format!(
                    "─── {} [compaction] ───\n",
                    format_timestamp(timestamp)
                ));
                out.push_str("Context compacted. Summary:\n");
                // Indent summary
                for line in summary.lines().take(30) {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
                let total_lines = summary.lines().count();
                if total_lines > 30 {
                    out.push_str(&format!("  ... ({} more lines)\n", total_lines - 30));
                }
                out.push('\n');
            }
        }
    }

    out
}

fn format_tool_interaction(out: &mut String, interaction: &ToolInteraction, opts: &FormatOptions) {
    // Format: [tool:name] key_arg
    let args_summary = summarize_tool_args(&interaction.name, &interaction.arguments);
    out.push_str(&format!("[tool:{}] {}\n", interaction.name, args_summary));

    if let Some(ref result) = interaction.result {
        let prefix = if result.is_error { "error: " } else { "" };
        let content = &result.content;

        if opts.raw || content.lines().count() <= MAX_TOOL_RESULT_LINES {
            out.push_str(&format!("  → {}{}\n", prefix, content.trim()));
        } else {
            // Truncate long results
            let lines: Vec<&str> = content.lines().collect();
            let show = MAX_TOOL_RESULT_LINES / 2;
            for line in &lines[..show] {
                out.push_str(&format!("  → {}\n", line));
            }
            out.push_str(&format!(
                "  ... ({} lines omitted)\n",
                lines.len() - MAX_TOOL_RESULT_LINES
            ));
            for line in &lines[lines.len() - show..] {
                out.push_str(&format!("  → {}\n", line));
            }
        }
    }
}

fn summarize_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    // Extract the most useful argument based on tool name
    match tool_name {
        "read" | "write" => args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.len() > 120 {
                    format!("{}...", &s[..120])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        "glob" => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("/{}/  in {}", pattern, path)
        }
        _ => {
            // Generic: show first string value
            if let Some(obj) = args.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        let display = if s.len() > 100 {
                            format!("{}...", &s[..100])
                        } else {
                            s.to_string()
                        };
                        return format!("{}={}", k, display);
                    }
                }
            }
            String::new()
        }
    }
}

fn format_timestamp(ts: &str) -> String {
    // Parse ISO 8601 and reformat cleanly
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|_| ts.to_string())
}
