//! Anthropic Claude API provider with SSE streaming

use crate::provider::{LlmError, LlmProvider, LlmResult, LlmStream};
use crate::types::{LlmRequest, StreamDelta, Usage};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: ANTHROPIC_API_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str { "anthropic" }

    fn models(&self) -> &[&str] {
        &[
            "claude-opus-4-6-20250929",
            "claude-opus-4-6",
            "claude-haiku-4-5-20251001",
        ]
    }

    async fn complete_stream(&self, request: LlmRequest) -> LlmResult<LlmStream> {
        // Heal any orphaned tool_use blocks before sending
        let healed_messages = crate::types::validate_and_heal_messages(&request.messages);

        let body = AnthropicRequest {
            model: request.model.clone(),
            messages: healed_messages.iter().map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: match &m.content {
                    crate::types::LlmContent::Text(s) => serde_json::json!(s),
                    crate::types::LlmContent::Blocks(blocks) => serde_json::to_value(blocks).unwrap_or_default(),
                },
            }).collect(),
            max_tokens: request.max_tokens.unwrap_or(8192),
            stream: true,
            system: request.system.clone(),
            tools: request.tools.as_ref().map(|tools| {
                tools.iter().map(|t| AnthropicTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                }).collect()
            }),
        };

        debug!("Anthropic request: model={}", body.model);

        let response = self.client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Anthropic error {}: {}", status, error_text);

            if status.as_u16() == 401 {
                return Err(LlmError::AuthFailed(error_text));
            } else if status.as_u16() == 429 {
                return Err(LlmError::RateLimited { retry_after_ms: 60000 });
            } else {
                return Err(LlmError::RequestFailed(format!("{}: {}", status, error_text)));
            }
        }

        let stream = parse_sse_stream(response.bytes_stream());
        Ok(Box::pin(stream))
    }
}

fn parse_sse_stream(
    bytes_stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> impl futures::Stream<Item = LlmResult<StreamDelta>> + Send {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut current_tool_id: Option<String> = None;

        tokio::pin!(bytes_stream);

        while let Some(chunk_result) = bytes_stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    yield Err(LlmError::StreamError(e.to_string()));
                    continue;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(event_end) = buffer.find("\n\n") {
                let event_str = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                let mut event_type = String::new();
                let mut event_data = String::new();

                for line in event_str.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event_type = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        event_data = rest.to_string();
                    }
                }

                if event_data.is_empty() { continue; }

                match event_type.as_str() {
                    "content_block_start" => {
                        if let Ok(data) = serde_json::from_str::<ContentBlockStart>(&event_data) {
                            match data.content_block {
                                ContentBlockType::ToolUse { id, name } => {
                                    current_tool_id = Some(id.clone());
                                                    yield Ok(StreamDelta::ToolCallStart { id, name });
                                }
                                ContentBlockType::Text { .. } => {}
                            }
                        }
                    }
                    "content_block_delta" => {
                        if let Ok(data) = serde_json::from_str::<ContentBlockDelta>(&event_data) {
                            match data.delta {
                                DeltaType::TextDelta { text } => {
                                    yield Ok(StreamDelta::Text(text));
                                }
                                DeltaType::ThinkingDelta { thinking } => {
                                    yield Ok(StreamDelta::Thinking(thinking));
                                }
                                DeltaType::InputJsonDelta { partial_json } => {
                                    if let Some(id) = &current_tool_id {
                                        yield Ok(StreamDelta::ToolCallDelta {
                                            id: id.clone(),
                                            arguments: partial_json,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    "content_block_stop" => {
                        if let Some(id) = current_tool_id.take() {
                            yield Ok(StreamDelta::ToolCallEnd { id });

                        }
                    }
                    "message_delta" => {
                        if let Ok(data) = serde_json::from_str::<MessageDelta>(&event_data) {
                            if let Some(stop_reason) = data.delta.stop_reason {
                                debug!("Message complete: stop_reason={}", stop_reason);
                            }
                        }
                    }
                    "message_stop" => {
                        yield Ok(StreamDelta::Done {
                            stop_reason: Some("end_turn".to_string()),
                            usage: None,
                        });
                    }
                    "error" => {
                        if let Ok(data) = serde_json::from_str::<ErrorEvent>(&event_data) {
                            yield Err(LlmError::StreamError(data.error.message));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Deserialize)]
struct ContentBlockStart {
    #[allow(dead_code)]
    index: u32,
    content_block: ContentBlockType,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlockType {
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(rename = "text")]
    #[allow(dead_code)]
    Text { text: String },
}

#[derive(Deserialize)]
struct ContentBlockDelta {
    #[allow(dead_code)]
    index: u32,
    delta: DeltaType,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum DeltaType {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize)]
struct MessageDelta {
    delta: MessageDeltaContent,
    #[allow(dead_code)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct MessageDeltaContent {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEvent {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}
