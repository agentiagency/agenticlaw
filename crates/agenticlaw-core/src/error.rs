//! Error types for Agenticlaw

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("authentication failed: {reason}")]
    AuthFailed { reason: String },

    #[error("connection closed: {0}")]
    ConnectionClosed(String),

    #[error("invalid message: {0}")]
    InvalidMessage(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("method not found: {0}")]
    MethodNotFound(String),

    #[error("llm error: {provider} - {message}")]
    LlmError { provider: String, message: String },

    #[error("tool error: {name} - {message}")]
    ToolError { name: String, message: String },

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn auth_failed(reason: impl Into<String>) -> Self {
        Self::AuthFailed {
            reason: reason.into(),
        }
    }

    pub fn llm_error(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::LlmError {
            provider: provider.into(),
            message: message.into(),
        }
    }

    pub fn tool_error(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ToolError {
            name: name.into(),
            message: message.into(),
        }
    }
}
