//! LLM types for requests and streaming responses

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// LLM request
#[derive(Clone, Debug, Serialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<LlmTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

impl Default for LlmRequest {
    fn default() -> Self {
        Self {
            model: "claude-opus-4-6-20250929".to_string(),
            messages: Vec::new(),
            tools: None,
            max_tokens: Some(8192),
            temperature: None,
            system: None,
        }
    }
}

/// Message in LLM conversation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: LlmContent,
}

/// Message content - can be string or array of blocks
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LlmContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl From<String> for LlmContent {
    fn from(s: String) -> Self {
        LlmContent::Text(s)
    }
}

impl From<&str> for LlmContent {
    fn from(s: &str) -> Self {
        LlmContent::Text(s.to_string())
    }
}

/// Content block types
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Tool definition
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Streaming delta from LLM
#[derive(Clone, Debug)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        arguments: String,
    },
    ToolCallEnd {
        id: String,
    },
    Done {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
    Error(String),
}

/// Token usage
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Accumulated tool call from streaming
#[derive(Clone, Debug, Default)]
pub struct AccumulatedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl AccumulatedToolCall {
    pub fn parse_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

/// Validate and heal message history before sending to Anthropic API.
///
/// Anthropic requires that every `tool_use` block in an assistant message
/// has exactly ONE corresponding `tool_result` block in the immediately
/// following user message. This function fixes two failure modes:
///
/// 1. **Missing tool_result** — tool call cancelled/crashed mid-execution.
///    Fix: inject error tool_result block.
/// 2. **Duplicate tool_result** — same tool_use_id appears multiple times.
///    Fix: keep only the first tool_result for each id, drop duplicates.
pub fn validate_and_heal_messages(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    let mut healed: Vec<LlmMessage> = Vec::with_capacity(messages.len());

    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        // Deduplicate tool_result blocks in user messages
        let cleaned = if msg.role == "user" {
            dedup_tool_results(msg)
        } else {
            msg.clone()
        };
        healed.push(cleaned);

        // If this is an assistant message, collect tool_use ids
        if msg.role == "assistant" {
            let tool_use_ids = extract_tool_use_ids(&msg.content);
            if !tool_use_ids.is_empty() {
                // Check the next message for matching tool_results
                let next = messages.get(i + 1);
                let existing_result_ids = next
                    .filter(|m| m.role == "user")
                    .map(|m| extract_tool_result_ids(&m.content))
                    .unwrap_or_default();

                let missing: Vec<String> = tool_use_ids
                    .iter()
                    .filter(|id| !existing_result_ids.contains(id))
                    .cloned()
                    .collect();

                if !missing.is_empty() {
                    warn!(
                        orphaned_tool_ids = ?missing,
                        "Healing orphaned tool_use blocks — injecting cancelled tool_results"
                    );
                    if let Some(next_msg) = next {
                        if next_msg.role == "user" {
                            // Append missing tool_results to the existing user message
                            i += 1;
                            let mut blocks = match &next_msg.content {
                                LlmContent::Blocks(b) => b.clone(),
                                LlmContent::Text(t) => vec![ContentBlock::Text { text: t.clone() }],
                            };
                            for id in &missing {
                                blocks.push(ContentBlock::ToolResult {
                                    tool_use_id: id.clone(),
                                    content: "[cancelled] Tool execution was interrupted."
                                        .to_string(),
                                    is_error: Some(true),
                                });
                            }
                            healed.push(LlmMessage {
                                role: "user".to_string(),
                                content: LlmContent::Blocks(blocks),
                            });
                            i += 1;
                            continue;
                        }
                    }
                    // No next message or next message is not user — inject a new user message
                    let blocks: Vec<ContentBlock> = missing
                        .iter()
                        .map(|id| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: "[cancelled] Tool execution was interrupted.".to_string(),
                            is_error: Some(true),
                        })
                        .collect();
                    healed.push(LlmMessage {
                        role: "user".to_string(),
                        content: LlmContent::Blocks(blocks),
                    });
                }
            }
        }

        i += 1;
    }

    healed
}

fn extract_tool_use_ids(content: &LlmContent) -> Vec<String> {
    match content {
        LlmContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    }
}

/// Deduplicate tool_result blocks in a user message.
/// Keeps only the first tool_result for each tool_use_id.
fn dedup_tool_results(msg: &LlmMessage) -> LlmMessage {
    match &msg.content {
        LlmContent::Blocks(blocks) => {
            let original_count = blocks.len();
            let mut seen_ids = std::collections::HashSet::new();
            let deduped: Vec<ContentBlock> = blocks
                .iter()
                .filter(|b| {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                        seen_ids.insert(tool_use_id.clone())
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            let removed = original_count - deduped.len();
            if removed > 0 {
                debug!(removed, "Deduplicated tool_result blocks");
            }
            LlmMessage {
                role: msg.role.clone(),
                content: LlmContent::Blocks(deduped),
            }
        }
        _ => msg.clone(),
    }
}

fn extract_tool_result_ids(content: &LlmContent) -> Vec<String> {
    match content {
        LlmContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    Some(tool_use_id.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => vec![],
    }
}
