//! `.ctx` — the native agent context format.
//!
//! A .ctx file is the source of truth for an agent session. It is human-readable,
//! agent-readable, and process-readable. JSONL is never persisted — it exists only
//! as a wire format for LLM API calls.
//!
//! # Format spec
//!
//! ```text
//! --- session: <id> ---
//! started: <ISO 8601>
//! cwd: <path>
//!
//! --- <ISO 8601> ---
//! [contents of SOUL.md]
//! [contents of AGENTS.md]
//! ...concatenated context files as first turn...
//!
//! --- <ISO 8601> ---
//! <up>
//! User message text here.
//! </up>
//!
//! --- <ISO 8601> ---
//! Assistant response text here.
//!
//! [tool:read] /path/to/file
//!   → file contents
//!
//! --- <ISO 8601> ---
//! <up>
//! [tool result contents — also <up> because it's input to the model]
//! </up>
//!
//! --- <ISO 8601> ---
//! The server runs on port 8080.
//!
//! --- <ISO 8601> [compaction] ---
//! Summary of compacted context here.
//! ```
//!
//! # Lifecycle
//!
//! 1. Session created → SOUL.md/AGENTS.md/etc concatenated as first turn in .ctx
//! 2. Uploaded to LLM API → .ctx JIT-wrapped into jsonl wire format (see `to_wire`)
//! 3. LLM responds → jsonl wrapper stripped, clean content appended to .ctx
//! 4. Tool results come back → appended as `<up>` blocks (input to model)
//! 5. .jsonl never touches disk — .ctx is the source of truth
//!
//! # `<up>` semantics
//!
//! `<up>` wraps anything that is **input to the model** from outside:
//! - User messages
//! - Tool call results
//! Everything outside `<up>` is model output.
//!
//! # Design principles
//!
//! - No JSON, no binary, no escaping
//! - Datetime lines (`--- <ts> ---`) are the only structural markers
//! - `<up></up>` tags delimit external input; bare text is model output
//! - Parsable back into SessionEvent for round-trip fidelity

use crate::session::Session;
use crate::transform::*;

// ---------------------------------------------------------------------------
// Emit: SessionEvent[] → clean context string
// ---------------------------------------------------------------------------

pub struct EmitOptions {
    pub include_thinking: bool,
    pub include_usage: bool,
    pub raw: bool,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_usage: false,
            raw: false,
        }
    }
}

const MAX_TOOL_RESULT_LINES: usize = 20;

pub fn emit(events: &[SessionEvent], opts: &EmitOptions) -> String {
    let mut out = String::new();

    for event in events {
        match event {
            SessionEvent::Header { id, timestamp, cwd, .. } => {
                out.push_str(&format!("--- session: {} ---\n", id));
                out.push_str(&format!("started: {}\n", timestamp));
                if let Some(cwd) = cwd {
                    out.push_str(&format!("cwd: {}\n", cwd));
                }
                out.push('\n');
            }
            SessionEvent::ModelChange { timestamp, provider, model_id } => {
                out.push_str(&format!(
                    "--- {} [model: {} ({})] ---\n\n",
                    timestamp, model_id, provider
                ));
            }
            SessionEvent::ThinkingLevelChange { timestamp, level } => {
                out.push_str(&format!(
                    "--- {} [thinking-level: {}] ---\n\n",
                    timestamp, level
                ));
            }
            SessionEvent::Turn(turn) => {
                out.push_str(&format!("--- {} ---\n", turn.timestamp));
                let is_user = turn.role == "user";
                if is_user {
                    out.push_str("<up>\n");
                }

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
                            emit_tool(&mut out, interaction, opts);
                        }
                    }
                }

                if is_user {
                    out.push_str("</up>\n");
                }

                if opts.include_usage {
                    if let Some(ref usage) = turn.usage {
                        let input = usage.input.unwrap_or(0);
                        let output = usage.output.unwrap_or(0);
                        let cache = usage.cache_read.unwrap_or(0);
                        let total = usage.total_tokens.unwrap_or(0);
                        out.push_str(&format!(
                            "[tokens: {} in, {} out, {} cached, {} total]\n",
                            input, output, cache, total
                        ));
                    }
                }
                out.push('\n');
            }
            SessionEvent::Compaction { timestamp, summary } => {
                out.push_str(&format!("--- {} [compaction] ---\n", timestamp));
                for line in summary.lines() {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
        }
    }

    out
}

fn emit_tool(out: &mut String, interaction: &ToolInteraction, opts: &EmitOptions) {
    let args_summary = summarize_tool_args(&interaction.name, &interaction.arguments);
    out.push_str(&format!("[tool:{}] {}\n", interaction.name, args_summary));

    if let Some(ref result) = interaction.result {
        let prefix = if result.is_error { "error: " } else { "" };
        let content = &result.content;

        if opts.raw || content.lines().count() <= MAX_TOOL_RESULT_LINES {
            out.push_str(&format!("  → {}{}\n", prefix, content.trim()));
        } else {
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
            .map(|s| if s.len() > 120 { format!("{}...", &s[..120]) } else { s.to_string() })
            .unwrap_or_default(),
        "glob" => args.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("/{}/  in {}", pattern, path)
        }
        _ => {
            if let Some(obj) = args.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        let display = if s.len() > 100 { format!("{}...", &s[..100]) } else { s.to_string() };
                        return format!("{}={}", k, display);
                    }
                }
            }
            String::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Parse: clean context string → SessionEvent[]
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

pub struct ParseResult {
    pub events: Vec<SessionEvent>,
    pub errors: Vec<ParseError>,
}

/// Parse a clean context file into session events.
pub fn parse(content: &str) -> ParseResult {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Session header: --- session: <id> ---
        if let Some(id) = line.strip_prefix("--- session: ").and_then(|s| s.strip_suffix(" ---")) {
            let id = id.to_string();
            i += 1;
            let mut timestamp = String::new();
            let mut cwd = None;
            while i < lines.len() && !lines[i].is_empty() {
                if let Some(ts) = lines[i].strip_prefix("started: ") {
                    timestamp = ts.to_string();
                } else if let Some(c) = lines[i].strip_prefix("cwd: ") {
                    cwd = Some(c.to_string());
                }
                i += 1;
            }
            events.push(SessionEvent::Header {
                version: 0, // clean context format doesn't carry version
                id,
                timestamp,
                cwd,
            });
            i += 1; // skip blank line
            continue;
        }

        // Separator line: --- <timestamp> [optional annotation] ---
        if line.starts_with("--- ") && line.ends_with(" ---") {
            let inner = &line[4..line.len() - 4];

            // Model change: --- <ts> [model: <id> (<provider>)] ---
            if let Some(rest) = extract_bracket(inner, "model: ") {
                let timestamp = inner[..inner.find('[').unwrap()].trim().to_string();
                // Parse "model_id (provider)"
                if let Some((model_id, provider)) = parse_model_annotation(rest) {
                    events.push(SessionEvent::ModelChange { timestamp, provider, model_id });
                }
                i += 1;
                if i < lines.len() && lines[i].is_empty() { i += 1; }
                continue;
            }

            // Thinking level change: --- <ts> [thinking-level: <level>] ---
            if let Some(rest) = extract_bracket(inner, "thinking-level: ") {
                let timestamp = inner[..inner.find('[').unwrap()].trim().to_string();
                events.push(SessionEvent::ThinkingLevelChange {
                    timestamp,
                    level: rest.to_string(),
                });
                i += 1;
                if i < lines.len() && lines[i].is_empty() { i += 1; }
                continue;
            }

            // Compaction: --- <ts> [compaction] ---
            if inner.ends_with("[compaction]") {
                let timestamp = inner[..inner.find('[').unwrap()].trim().to_string();
                i += 1;
                let mut summary_lines = Vec::new();
                while i < lines.len() && !lines[i].starts_with("--- ") {
                    if lines[i].is_empty() && i + 1 < lines.len() && lines[i + 1].starts_with("--- ") {
                        break;
                    }
                    summary_lines.push(lines[i]);
                    i += 1;
                }
                // Skip trailing blank
                if i < lines.len() && lines[i].is_empty() { i += 1; }
                events.push(SessionEvent::Compaction {
                    timestamp,
                    summary: summary_lines.join("\n"),
                });
                continue;
            }

            // Regular turn: --- <timestamp> ---
            let timestamp = inner.to_string();
            i += 1;

            // Check for <up> tag (user turn)
            let is_user = i < lines.len() && lines[i] == "<up>";
            if is_user { i += 1; }

            let mut contents = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                // End of user turn
                if is_user && l == "</up>" {
                    i += 1;
                    break;
                }
                // Next separator
                if l.starts_with("--- ") && l.ends_with(" ---") {
                    break;
                }
                // Blank line after non-user turn = end of turn
                if !is_user && l.is_empty() && (i + 1 >= lines.len() || lines[i + 1].starts_with("--- ")) {
                    i += 1;
                    break;
                }

                // Thinking block
                if let Some(thinking) = l.strip_prefix("[thinking] ") {
                    contents.push(TurnContent::Thinking(thinking.to_string()));
                    i += 1;
                    continue;
                }

                // Tool interaction
                if l.starts_with("[tool:") {
                    let (interaction, next_i) = parse_tool_block(&lines, i);
                    contents.push(TurnContent::Tool(interaction));
                    i = next_i;
                    continue;
                }

                // Token usage line (skip, metadata only)
                if l.starts_with("[tokens: ") {
                    i += 1;
                    continue;
                }

                // Regular text
                if !l.is_empty() || (!is_user && !contents.is_empty()) {
                    // Append to last text block or create new one
                    if let Some(TurnContent::Text(existing)) = contents.last_mut() {
                        existing.push('\n');
                        existing.push_str(l);
                    } else if !l.is_empty() {
                        contents.push(TurnContent::Text(l.to_string()));
                    }
                }
                i += 1;
            }

            // Skip trailing blank after turn
            if i < lines.len() && lines[i].is_empty() { i += 1; }

            if !contents.is_empty() {
                let role = if is_user { "user" } else { "assistant" }.to_string();
                events.push(SessionEvent::Turn(Turn {
                    timestamp,
                    role,
                    contents,
                    usage: None,
                }));
            }
            continue;
        }

        // Unrecognized line outside a turn
        if !line.is_empty() {
            errors.push(ParseError {
                line: i + 1,
                message: format!("Unexpected content outside turn: {}", line),
            });
        }
        i += 1;
    }

    ParseResult { events, errors }
}

fn extract_bracket<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    let inner = &s[start + 1..end];
    inner.strip_prefix(prefix)
}

fn parse_model_annotation(s: &str) -> Option<(String, String)> {
    // "claude-opus-4-6 (anthropic)" → ("claude-opus-4-6", "anthropic")
    let paren_start = s.find('(')?;
    let paren_end = s.find(')')?;
    let model_id = s[..paren_start].trim().to_string();
    let provider = s[paren_start + 1..paren_end].to_string();
    Some((model_id, provider))
}

fn parse_tool_block(lines: &[&str], start: usize) -> (ToolInteraction, usize) {
    let line = lines[start];
    // [tool:name] args_summary
    let colon = line.find(':').unwrap_or(5);
    let bracket_end = line.find(']').unwrap_or(line.len());
    let name = line[colon + 1..bracket_end].to_string();
    let args_text = line[bracket_end + 1..].trim().to_string();

    let mut i = start + 1;
    let mut result_lines = Vec::new();
    let mut is_error = false;

    while i < lines.len() {
        let l = lines[i];
        if let Some(content) = l.strip_prefix("  → ") {
            if result_lines.is_empty() && content.starts_with("error: ") {
                is_error = true;
                result_lines.push(content.strip_prefix("error: ").unwrap().to_string());
            } else {
                result_lines.push(content.to_string());
            }
            i += 1;
        } else if l.starts_with("  ... (") && l.ends_with(" lines omitted)") {
            // Truncation marker — skip
            i += 1;
        } else {
            break;
        }
    }

    let result = if result_lines.is_empty() {
        None
    } else {
        Some(ToolResultInfo {
            content: result_lines.join("\n"),
            is_error,
        })
    };

    // Build a simple args JSON from the summary (best-effort)
    let arguments = if !args_text.is_empty() {
        match name.as_str() {
            "read" | "write" => serde_json::json!({"file_path": args_text}),
            "bash" => serde_json::json!({"command": args_text}),
            "glob" => serde_json::json!({"pattern": args_text}),
            _ => serde_json::json!({"summary": args_text}),
        }
    } else {
        serde_json::json!({})
    };

    (ToolInteraction { name, arguments, result }, i)
}

// ---------------------------------------------------------------------------
// CleanContextSession: Session trait implementation
// ---------------------------------------------------------------------------

pub struct CleanContextSession {
    id: String,
    timestamp: String,
    cwd: Option<String>,
    events: Vec<SessionEvent>,
    pub parse_errors: Vec<ParseError>,
}

impl CleanContextSession {
    pub fn from_str(content: &str) -> Self {
        let result = parse(content);

        let (id, timestamp, cwd) = result.events.iter().find_map(|e| {
            if let SessionEvent::Header { id, timestamp, cwd, .. } = e {
                Some((id.clone(), timestamp.clone(), cwd.clone()))
            } else {
                None
            }
        }).unwrap_or_default();

        CleanContextSession {
            id,
            timestamp,
            cwd,
            events: result.events,
            parse_errors: result.errors,
        }
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::from_str(&content))
    }
}

impl Session for CleanContextSession {
    fn id(&self) -> &str { &self.id }
    fn timestamp(&self) -> &str { &self.timestamp }
    fn cwd(&self) -> Option<&str> { self.cwd.as_deref() }
    fn events(&self) -> &[SessionEvent] { &self.events }
}

// ---------------------------------------------------------------------------
// Init: concatenate context files into the first .ctx turn
// ---------------------------------------------------------------------------

/// Create the initial .ctx content for a new session by concatenating context files.
/// Each file's content becomes part of the first turn (the system context).
pub fn init_session(
    session_id: &str,
    timestamp: &str,
    cwd: Option<&str>,
    context_files: &[&str],  // file contents in order
) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- session: {} ---\n", session_id));
    out.push_str(&format!("started: {}\n", timestamp));
    if let Some(cwd) = cwd {
        out.push_str(&format!("cwd: {}\n", cwd));
    }
    out.push('\n');

    // Context files become the first turn — plain text, no <up> tags
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

    out
}

// ---------------------------------------------------------------------------
// Wire format: .ctx events → JSONL for LLM API transport
// ---------------------------------------------------------------------------

/// Convert session events to JSONL wire format for LLM API calls.
/// Each line is a self-contained JSON object. Preloads become system messages.
pub fn to_wire(events: &[SessionEvent]) -> String {
    let mut lines = Vec::new();

    for event in events {
        match event {
            SessionEvent::Header { version, id, timestamp, cwd } => {
                let mut obj = serde_json::json!({
                    "type": "session",
                    "version": version,
                    "id": id,
                    "timestamp": timestamp,
                });
                if let Some(cwd) = cwd {
                    obj["cwd"] = serde_json::json!(cwd);
                }
                lines.push(serde_json::to_string(&obj).unwrap());
            }
            SessionEvent::ModelChange { timestamp, provider, model_id } => {
                let obj = serde_json::json!({
                    "type": "model_change",
                    "id": format!("mc-{}", &timestamp[..19].replace(':', "")),
                    "timestamp": timestamp,
                    "provider": provider,
                    "modelId": model_id,
                });
                lines.push(serde_json::to_string(&obj).unwrap());
            }
            SessionEvent::Turn(turn) => {
                let mut content_blocks = Vec::new();
                for c in &turn.contents {
                    match c {
                        TurnContent::Text(text) => {
                            content_blocks.push(serde_json::json!({
                                "type": "text",
                                "text": text,
                            }));
                        }
                        TurnContent::Thinking(thinking) => {
                            content_blocks.push(serde_json::json!({
                                "type": "thinking",
                                "thinking": thinking,
                            }));
                        }
                        TurnContent::Tool(interaction) => {
                            content_blocks.push(serde_json::json!({
                                "type": "toolCall",
                                "id": format!("tc-{}", interaction.name),
                                "name": interaction.name,
                                "arguments": interaction.arguments,
                            }));
                        }
                    }
                }
                let obj = serde_json::json!({
                    "type": "message",
                    "id": format!("turn-{}", &turn.timestamp[..19].replace(':', "")),
                    "timestamp": turn.timestamp,
                    "message": {
                        "role": turn.role,
                        "content": content_blocks,
                    }
                });
                lines.push(serde_json::to_string(&obj).unwrap());

                // Emit tool results as separate messages (wire format convention)
                for c in &turn.contents {
                    if let TurnContent::Tool(interaction) = c {
                        if let Some(ref result) = interaction.result {
                            let obj = serde_json::json!({
                                "type": "message",
                                "id": format!("tr-{}", interaction.name),
                                "timestamp": turn.timestamp,
                                "message": {
                                    "role": "toolResult",
                                    "toolCallId": format!("tc-{}", interaction.name),
                                    "toolName": interaction.name,
                                    "content": [{"type": "text", "text": result.content}],
                                    "isError": result.is_error,
                                }
                            });
                            lines.push(serde_json::to_string(&obj).unwrap());
                        }
                    }
                }
            }
            SessionEvent::Compaction { timestamp, summary } => {
                let obj = serde_json::json!({
                    "type": "compaction",
                    "id": format!("comp-{}", &timestamp[..19].replace(':', "")),
                    "timestamp": timestamp,
                    "summary": summary,
                });
                lines.push(serde_json::to_string(&obj).unwrap());
            }
            SessionEvent::ThinkingLevelChange { timestamp, level } => {
                let obj = serde_json::json!({
                    "type": "thinking_level_change",
                    "id": format!("tl-{}", &timestamp[..19].replace(':', "")),
                    "timestamp": timestamp,
                    "thinkingLevel": level,
                });
                lines.push(serde_json::to_string(&obj).unwrap());
            }
        }
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Strip JSONL wire format response and return a clean .ctx turn to append.
/// Takes a single JSONL assistant response line and returns the .ctx text to append.
pub fn from_wire_response(jsonl_line: &str) -> Option<String> {
    let record: serde_json::Value = serde_json::from_str(jsonl_line).ok()?;
    let msg = record.get("message")?;
    let role = msg.get("role")?.as_str()?;
    if role != "assistant" { return None; }

    let timestamp = record.get("timestamp")?.as_str()?;
    let mut out = format!("--- {} ---\n", timestamp);

    if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
                Some("thinking") => {
                    if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                        out.push_str(&format!("[thinking] {}\n", thinking));
                    }
                }
                Some("toolCall") => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                    let args = block.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                    let summary = summarize_tool_args(name, &args);
                    out.push_str(&format!("[tool:{}] {}\n", name, summary));
                }
                _ => {}
            }
        }
    }

    out.push('\n');
    Some(out)
}
