use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Session(SessionRecord),
    ModelChange(ModelChangeRecord),
    ThinkingLevelChange(ThinkingLevelChangeRecord),
    Message(MessageRecord),
    Custom(CustomRecord),
    Compaction(CompactionRecord),
}

#[derive(Debug, Deserialize)]
pub struct SessionRecord {
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: MessageBody,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageBody {
    pub role: String,
    pub content: Option<MessageContent>,
    pub timestamp: Option<u64>,
    // Assistant-only
    pub model: Option<String>,
    pub provider: Option<String>,
    pub usage: Option<Usage>,
    // ToolResult-only
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub is_error: Option<bool>,
    pub details: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Blocks(Vec<ContentBlock>),
    Text(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        thinking_signature: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost: Option<Cost>,
}

#[derive(Debug, Deserialize)]
pub struct Cost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub total: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    pub data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
}
