//! LLM Provider trait

use crate::types::{LlmRequest, StreamDelta};
use futures::Stream;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Result type for LLM operations
pub type LlmResult<T> = Result<T, LlmError>;

/// LLM error types
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("rate limited: retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("context overflow: {0}")]
    ContextOverflow(String),

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("stream error: {0}")]
    StreamError(String),

    #[error("cancelled")]
    Cancelled,

    #[error("network error: {0}")]
    NetworkError(#[from] reqwest::Error),
}

/// Stream type for LLM responses
pub type LlmStream = Pin<Box<dyn Stream<Item = LlmResult<StreamDelta>> + Send>>;

/// LLM Provider trait
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn models(&self) -> &[&str];

    fn supports_model(&self, model: &str) -> bool {
        self.models()
            .iter()
            .any(|m| *m == model || model.starts_with(m))
    }

    /// Stream a completion response. If `cancel` is provided and triggered,
    /// the underlying HTTP connection is dropped and the stream yields `LlmError::Cancelled`.
    async fn complete_stream(
        &self,
        request: LlmRequest,
        cancel: Option<CancellationToken>,
    ) -> LlmResult<LlmStream>;
}
