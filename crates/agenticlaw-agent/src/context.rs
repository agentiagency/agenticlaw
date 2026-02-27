//! Context window management with token counting

use agenticlaw_llm::{ContentBlock, LlmContent, LlmMessage};

const CHARS_PER_TOKEN: f32 = 4.0;

pub struct ContextManager {
    max_tokens: usize,
    system_tokens: usize,
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            system_tokens: 0,
        }
    }

    pub fn estimate_tokens(text: &str) -> usize {
        (text.len() as f32 / CHARS_PER_TOKEN).ceil() as usize
    }

    pub fn message_tokens(message: &LlmMessage) -> usize {
        let content_tokens = match &message.content {
            LlmContent::Text(s) => Self::estimate_tokens(s),
            LlmContent::Blocks(blocks) => blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => Self::estimate_tokens(text),
                    ContentBlock::ToolUse { name, input, .. } => {
                        Self::estimate_tokens(name) + Self::estimate_tokens(&input.to_string())
                    }
                    ContentBlock::ToolResult { content, .. } => Self::estimate_tokens(content),
                })
                .sum(),
        };
        content_tokens + 10
    }

    pub fn set_system(&mut self, system: &str) {
        self.system_tokens = Self::estimate_tokens(system);
    }

    pub fn calculate_total(&self, messages: &[LlmMessage]) -> usize {
        let message_tokens: usize = messages.iter().map(Self::message_tokens).sum();
        self.system_tokens + message_tokens
    }

    pub fn compact(&self, messages: &mut Vec<LlmMessage>) {
        if messages.is_empty() {
            return;
        }
        let total = self.calculate_total(messages);
        if total <= self.max_tokens {
            return;
        }
        let target = (self.max_tokens as f32 * 0.75) as usize;
        while messages.len() > 2 && self.calculate_total(messages) > target {
            messages.remove(1);
        }
        tracing::info!(
            "Compacted context: {} messages, ~{} tokens",
            messages.len(),
            self.calculate_total(messages)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_estimation() {
        assert_eq!(ContextManager::estimate_tokens("hello"), 2);
        assert_eq!(ContextManager::estimate_tokens("hello world"), 3);
    }
}
