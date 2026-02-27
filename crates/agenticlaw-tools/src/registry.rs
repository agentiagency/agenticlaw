//! Tool registry and trait definitions
//!
//! Each tool is a self-contained module implementing the Tool trait.
//! Tools can be added/removed by editing the tools/ directory and
//! the create_default_registry() function in lib.rs.

use agenticlaw_llm::LlmTool;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum ToolResult {
    Text(String),
    Json(Value),
    Error(String),
}

impl ToolResult {
    pub fn text(s: impl Into<String>) -> Self { Self::Text(s.into()) }
    pub fn error(s: impl Into<String>) -> Self { Self::Error(s.into()) }

    pub fn to_content_string(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            Self::Error(e) => format!("Error: {}", e),
        }
    }

    pub fn is_error(&self) -> bool { matches!(self, Self::Error(_)) }
}

/// The Tool trait — implement this to add a new capability.
///
/// Each tool is a standalone unit that can be registered with a ToolRegistry.
/// To add a new tool: create a file in tools/, implement this trait, register
/// it in create_default_registry().
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g. "bash", "read", "glob").
    fn name(&self) -> &str;

    /// Human-readable description sent to the LLM.
    fn description(&self) -> &str;

    /// System prompt fragment for this tool (injected into LLM context).
    fn prompt(&self) -> &str { "" }

    /// JSON Schema for input parameters.
    fn input_schema(&self) -> Value;

    /// Whether this tool only reads state (no side effects).
    fn is_read_only(&self) -> bool { false }

    /// Whether this tool is currently enabled.
    fn is_enabled(&self) -> bool { true }

    /// Execute the tool with the given arguments.
    async fn execute(&self, args: Value) -> ToolResult;

    /// Execute with cancellation support. Default: race execute() against cancellation.
    /// Tools that manage child processes (like BashTool) should override this to
    /// kill the process on cancellation.
    async fn execute_cancellable(
        &self,
        args: Value,
        cancel: CancellationToken,
    ) -> ToolResult {
        tokio::select! {
            result = self.execute(args) => result,
            _ = cancel.cancelled() => ToolResult::text("[cancelled]"),
        }
    }

    /// Convert to the LLM tool definition format.
    fn to_llm_tool(&self) -> LlmTool {
        LlmTool {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self { Self::new() }
}

impl ToolRegistry {
    pub fn new() -> Self { Self { tools: HashMap::new() } }

    /// Register a tool. Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    /// Remove a tool by name.
    pub fn remove(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub async fn execute(&self, name: &str, args: Value) -> ToolResult {
        match self.tools.get(name) {
            Some(tool) if tool.is_enabled() => tool.execute(args).await,
            Some(_) => ToolResult::Error(format!("Tool '{}' is disabled", name)),
            None => ToolResult::Error(format!("Tool not found: {}", name)),
        }
    }

    /// Execute a tool with cancellation support.
    pub async fn execute_cancellable(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> ToolResult {
        match self.tools.get(name) {
            Some(tool) if tool.is_enabled() => tool.execute_cancellable(args, cancel).await,
            Some(_) => ToolResult::Error(format!("Tool '{}' is disabled", name)),
            None => ToolResult::Error(format!("Tool not found: {}", name)),
        }
    }

    /// Get LLM tool definitions for all enabled tools.
    pub fn get_definitions(&self) -> Vec<LlmTool> {
        self.tools.values()
            .filter(|t| t.is_enabled())
            .map(|t| t.to_llm_tool())
            .collect()
    }

    /// Get system prompt fragments from all enabled tools.
    pub fn combined_prompts(&self) -> String {
        self.tools.values()
            .filter(|t| t.is_enabled())
            .map(|t| t.prompt())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn list(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// List only read-only tools.
    pub fn list_read_only(&self) -> Vec<&str> {
        self.tools.iter()
            .filter(|(_, t)| t.is_read_only())
            .map(|(k, _)| k.as_str())
            .collect()
    }
}
