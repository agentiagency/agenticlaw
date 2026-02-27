//! LLM types for requests and streaming responses

use serde::{Deserialize, Serialize};

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
    fn from(s: String) -> Self { LlmContent::Text(s) }
}

impl From<&str> for LlmContent {
    fn from(s: &str) -> Self { LlmContent::Text(s.to_string()) }
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
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, arguments: String },
    ToolCallEnd { id: String },
    Done { stop_reason: Option<String>, usage: Option<Usage> },
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
