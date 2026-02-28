//! MockLlmProvider — deterministic LLM responses for testing
//!
//! Implements the LlmProvider trait from agenticlaw-llm, returning canned
//! responses that exercise specific tool calls for policy testing.

use async_stream::stream;
use futures_util::stream::Stream;
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock behavior configuration
#[derive(Clone, Debug)]
pub enum MockBehavior {
    /// Return a text-only response
    Text(String),
    /// Return a tool_use call with given name and args
    ToolCall { name: String, args: Value },
    /// Return multiple tool_use calls
    MultiToolCall(Vec<(String, Value)>),
    /// Return text followed by a tool call
    TextThenTool {
        text: String,
        tool_name: String,
        tool_args: Value,
    },
    /// Return malformed JSON (for fuzz testing)
    Malformed(String),
    /// Return an error
    Error(String),
}

/// A sequence of behaviors — each call to complete_stream pops the next one.
/// If the sequence is exhausted, returns a default text response.
pub struct MockProvider {
    behaviors: Mutex<Vec<MockBehavior>>,
    default_behavior: MockBehavior,
    call_count: Mutex<usize>,
}

impl MockProvider {
    /// Create a mock that always returns the same behavior
    pub fn constant(behavior: MockBehavior) -> Self {
        Self {
            behaviors: Mutex::new(Vec::new()),
            default_behavior: behavior,
            call_count: Mutex::new(0),
        }
    }

    /// Create a mock with a sequence of behaviors (consumed in order)
    pub fn sequence(behaviors: Vec<MockBehavior>) -> Self {
        Self {
            behaviors: Mutex::new(behaviors),
            default_behavior: MockBehavior::Text("(mock: sequence exhausted)".into()),
            call_count: Mutex::new(0),
        }
    }

    /// Create a mock that always tries to call a denied tool (for adversarial testing)
    pub fn adversarial_tool(tool_name: &str, args: Value) -> Self {
        Self::constant(MockBehavior::ToolCall {
            name: tool_name.to_string(),
            args,
        })
    }

    /// Get the number of calls made
    pub async fn call_count(&self) -> usize {
        *self.call_count.lock().await
    }

    async fn next_behavior(&self) -> MockBehavior {
        let mut count = self.call_count.lock().await;
        *count += 1;

        let mut behaviors = self.behaviors.lock().await;
        if behaviors.is_empty() {
            self.default_behavior.clone()
        } else {
            behaviors.remove(0)
        }
    }
}

/// Stream delta types matching agenticlaw-llm's StreamDelta
/// We redefine them here to avoid depending on agenticlaw-llm in operator
#[derive(Debug, Clone)]
pub enum MockStreamDelta {
    Text(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, arguments: String },
    ToolCallEnd { id: String },
    Done { stop_reason: Option<String> },
    Error(String),
}

pub type MockStream = Pin<Box<dyn Stream<Item = Result<MockStreamDelta, String>> + Send>>;

impl MockProvider {
    /// Generate a stream of deltas for the given behavior
    pub async fn complete_stream_mock(&self) -> MockStream {
        let behavior = self.next_behavior().await;

        Box::pin(stream! {
            match behavior {
                MockBehavior::Text(text) => {
                    // Stream text in chunks like a real LLM
                    for chunk in text.as_bytes().chunks(20) {
                        let s = String::from_utf8_lossy(chunk).to_string();
                        yield Ok(MockStreamDelta::Text(s));
                    }
                    yield Ok(MockStreamDelta::Done { stop_reason: Some("end_turn".into()) });
                }

                MockBehavior::ToolCall { name, args } => {
                    let id = format!("toolu_mock_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                    yield Ok(MockStreamDelta::ToolCallStart { id: id.clone(), name });
                    yield Ok(MockStreamDelta::ToolCallDelta {
                        id: id.clone(),
                        arguments: serde_json::to_string(&args).unwrap_or_default(),
                    });
                    yield Ok(MockStreamDelta::ToolCallEnd { id });
                    yield Ok(MockStreamDelta::Done { stop_reason: Some("tool_use".into()) });
                }

                MockBehavior::MultiToolCall(tools) => {
                    for (name, args) in tools {
                        let id = format!("toolu_mock_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                        yield Ok(MockStreamDelta::ToolCallStart { id: id.clone(), name });
                        yield Ok(MockStreamDelta::ToolCallDelta {
                            id: id.clone(),
                            arguments: serde_json::to_string(&args).unwrap_or_default(),
                        });
                        yield Ok(MockStreamDelta::ToolCallEnd { id });
                    }
                    yield Ok(MockStreamDelta::Done { stop_reason: Some("tool_use".into()) });
                }

                MockBehavior::TextThenTool { text, tool_name, tool_args } => {
                    yield Ok(MockStreamDelta::Text(text));
                    let id = format!("toolu_mock_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                    yield Ok(MockStreamDelta::ToolCallStart { id: id.clone(), name: tool_name });
                    yield Ok(MockStreamDelta::ToolCallDelta {
                        id: id.clone(),
                        arguments: serde_json::to_string(&tool_args).unwrap_or_default(),
                    });
                    yield Ok(MockStreamDelta::ToolCallEnd { id });
                    yield Ok(MockStreamDelta::Done { stop_reason: Some("tool_use".into()) });
                }

                MockBehavior::Malformed(data) => {
                    yield Ok(MockStreamDelta::Text(data));
                    yield Ok(MockStreamDelta::Done { stop_reason: Some("end_turn".into()) });
                }

                MockBehavior::Error(msg) => {
                    yield Err(msg);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use serde_json::json;

    #[tokio::test]
    async fn mock_text_response() {
        let mock = MockProvider::constant(MockBehavior::Text("hello world".into()));
        let mut stream = mock.complete_stream_mock().await;

        let mut text = String::new();
        let mut got_done = false;
        while let Some(Ok(delta)) = stream.next().await {
            match delta {
                MockStreamDelta::Text(t) => text.push_str(&t),
                MockStreamDelta::Done { .. } => { got_done = true; break; }
                _ => {}
            }
        }
        assert_eq!(text, "hello world");
        assert!(got_done);
        assert_eq!(mock.call_count().await, 1);
    }

    #[tokio::test]
    async fn mock_tool_call() {
        let mock = MockProvider::adversarial_tool("bash", json!({"command": "rm -rf /"}));
        let mut stream = mock.complete_stream_mock().await;

        let mut tool_name = String::new();
        let mut tool_args = String::new();
        while let Some(Ok(delta)) = stream.next().await {
            match delta {
                MockStreamDelta::ToolCallStart { name, .. } => tool_name = name,
                MockStreamDelta::ToolCallDelta { arguments, .. } => tool_args = arguments,
                MockStreamDelta::Done { .. } => break,
                _ => {}
            }
        }
        assert_eq!(tool_name, "bash");
        assert!(tool_args.contains("rm -rf"));
    }

    #[tokio::test]
    async fn mock_sequence_exhaustion() {
        let mock = MockProvider::sequence(vec![
            MockBehavior::Text("first".into()),
            MockBehavior::Text("second".into()),
        ]);

        // First call
        let mut s = mock.complete_stream_mock().await;
        let mut t = String::new();
        while let Some(Ok(d)) = s.next().await {
            match d {
                MockStreamDelta::Text(x) => t.push_str(&x),
                MockStreamDelta::Done { .. } => break,
                _ => {}
            }
        }
        assert_eq!(t, "first");

        // Second call
        let mut s = mock.complete_stream_mock().await;
        let mut t = String::new();
        while let Some(Ok(d)) = s.next().await {
            match d {
                MockStreamDelta::Text(x) => t.push_str(&x),
                MockStreamDelta::Done { .. } => break,
                _ => {}
            }
        }
        assert_eq!(t, "second");

        // Third call — exhausted, gets default
        let mut s = mock.complete_stream_mock().await;
        let mut t = String::new();
        while let Some(Ok(d)) = s.next().await {
            match d {
                MockStreamDelta::Text(x) => t.push_str(&x),
                MockStreamDelta::Done { .. } => break,
                _ => {}
            }
        }
        assert!(t.contains("sequence exhausted"));
        assert_eq!(mock.call_count().await, 3);
    }

    #[tokio::test]
    async fn mock_error() {
        let mock = MockProvider::constant(MockBehavior::Error("API down".into()));
        let mut stream = mock.complete_stream_mock().await;
        let result = stream.next().await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn mock_multi_tool() {
        let mock = MockProvider::constant(MockBehavior::MultiToolCall(vec![
            ("read".into(), json!({"file_path": "/etc/shadow"})),
            ("bash".into(), json!({"command": "cat /etc/passwd"})),
        ]));
        let mut stream = mock.complete_stream_mock().await;

        let mut tool_names = Vec::new();
        while let Some(Ok(delta)) = stream.next().await {
            match delta {
                MockStreamDelta::ToolCallStart { name, .. } => tool_names.push(name),
                MockStreamDelta::Done { .. } => break,
                _ => {}
            }
        }
        assert_eq!(tool_names, vec!["read", "bash"]);
    }
}
