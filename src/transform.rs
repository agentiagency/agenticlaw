use std::collections::HashMap;

use crate::types::*;

/// A linked turn: an assistant tool call paired with its result.
pub struct ToolInteraction {
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: Option<ToolResultInfo>,
}

pub struct ToolResultInfo {
    pub content: String,
    pub is_error: bool,
}

pub enum TurnContent {
    Text(String),
    Thinking(String),
    Tool(ToolInteraction),
}

pub struct Turn {
    pub timestamp: String,
    pub role: String,
    pub contents: Vec<TurnContent>,
    pub usage: Option<Usage>,
}

pub enum SessionEvent {
    Header {
        version: u32,
        id: String,
        timestamp: String,
        cwd: Option<String>,
    },
    ModelChange {
        timestamp: String,
        provider: String,
        model_id: String,
    },
    ThinkingLevelChange {
        timestamp: String,
        level: String,
    },
    Turn(Turn),
    Compaction {
        timestamp: String,
        summary: String,
    },
}

/// Transform parsed records into a sequence of events with tool calls linked to results.
pub fn transform(records: Vec<Record>) -> Vec<SessionEvent> {
    // First pass: index tool results by toolCallId
    let mut tool_results: HashMap<String, ToolResultInfo> = HashMap::new();
    for record in &records {
        if let Record::Message(msg) = record {
            if msg.message.role == "toolResult" {
                if let Some(ref call_id) = msg.message.tool_call_id {
                    let content = extract_text_content(&msg.message.content);
                    let is_error = msg.message.is_error.unwrap_or(false);
                    tool_results.insert(call_id.clone(), ToolResultInfo { content, is_error });
                }
            }
        }
    }

    // Second pass: build events
    let mut events = Vec::new();

    for record in records {
        match record {
            Record::Session(s) => {
                events.push(SessionEvent::Header {
                    version: s.version,
                    id: s.id,
                    timestamp: s.timestamp,
                    cwd: s.cwd,
                });
            }
            Record::ModelChange(m) => {
                events.push(SessionEvent::ModelChange {
                    timestamp: m.timestamp,
                    provider: m.provider,
                    model_id: m.model_id,
                });
            }
            Record::ThinkingLevelChange(t) => {
                events.push(SessionEvent::ThinkingLevelChange {
                    timestamp: t.timestamp,
                    level: t.thinking_level,
                });
            }
            Record::Message(msg) => {
                // Skip toolResult messages — they're inlined into tool calls
                if msg.message.role == "toolResult" {
                    continue;
                }

                let mut contents = Vec::new();
                if let Some(ref content) = msg.message.content {
                    let blocks = match content {
                        MessageContent::Blocks(b) => b,
                        MessageContent::Text(t) => {
                            contents.push(TurnContent::Text(t.clone()));
                            &Vec::new() as &Vec<ContentBlock>
                        }
                    };
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    contents.push(TurnContent::Text(trimmed.to_string()));
                                }
                            }
                            ContentBlock::Thinking { thinking, .. } => {
                                contents.push(TurnContent::Thinking(thinking.clone()));
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                let result = tool_results.remove(id);
                                contents.push(TurnContent::Tool(ToolInteraction {
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                    result,
                                }));
                            }
                            ContentBlock::Unknown => {}
                        }
                    }
                }

                if !contents.is_empty() {
                    events.push(SessionEvent::Turn(Turn {
                        timestamp: msg.timestamp,
                        role: msg.message.role,
                        contents,
                        usage: msg.message.usage,
                    }));
                }
            }
            Record::Compaction(c) => {
                events.push(SessionEvent::Compaction {
                    timestamp: c.timestamp,
                    summary: c.summary,
                });
            }
            Record::Custom(_) => {
                // Custom events are metadata — skip in plaintext output
            }
        }
    }

    events
}

fn extract_text_content(content: &Option<MessageContent>) -> String {
    match content {
        Some(MessageContent::Blocks(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let ContentBlock::Text { text } = block {
                    parts.push(text.as_str());
                }
            }
            parts.join("\n")
        }
        Some(MessageContent::Text(t)) => t.clone(),
        None => String::new(),
    }
}
