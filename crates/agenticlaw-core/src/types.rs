//! Core types for Agenticlaw

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Session identifier - cheaply cloneable
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct SessionKey(Arc<str>);

impl SessionKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Arc::from(s.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for SessionKey {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for SessionKey {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Message role
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A message in a conversation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool call from the assistant
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Tool definition for LLM
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
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
    Done,
    Error(String),
}

/// Gateway configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub bind: BindMode,
    #[serde(default)]
    pub auth: AuthConfig,
}

fn default_port() -> u16 {
    18789
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            bind: BindMode::default(),
            auth: AuthConfig::default(),
        }
    }
}

/// Bind mode for the gateway
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindMode {
    Loopback,
    #[default]
    Lan,
}

impl BindMode {
    pub fn to_addr(&self) -> &str {
        match self {
            BindMode::Loopback => "127.0.0.1",
            BindMode::Lan => "0.0.0.0",
        }
    }
}

/// Authentication configuration
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    pub token: Option<String>,
}

/// Authentication mode
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    Token,
    None,
}
