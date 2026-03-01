//! Subagent lifecycle tool — pause, resume, kill, query, list running subagents.
//!
//! Exposes the SubagentRegistry to the LLM so the agent (or HITL) can
//! manage child agent lifecycles.

use crate::registry::{Tool, ToolResult};
use crate::tools::spawn::SubagentRegistryHandle;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SubagentTool {
    registry: Arc<RwLock<Option<SubagentRegistryHandle>>>,
}

impl SubagentTool {
    pub fn new(registry: Arc<RwLock<Option<SubagentRegistryHandle>>>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        "Manage subagent lifecycles: list, query, pause, resume, or kill running subagents."
    }

    fn prompt(&self) -> &str {
        "Use the subagent tool to control child agents:\n\
         - list: show all subagents with status\n\
         - query <name>: get detailed status of a specific subagent\n\
         - pause <name>: suspend a running subagent (recursive to children)\n\
         - resume <name>: resume a paused subagent (recursive to children)\n\
         - kill <name>: terminate a subagent (recursive to children)\n\n\
         Names are purpose-hash format (e.g. 'fix-slider-css-a3f9b'). Prefix matching works."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["list", "query", "pause", "resume", "kill"],
                    "description": "Lifecycle command"
                },
                "name": {
                    "type": "string",
                    "description": "Subagent name or prefix (required for query/pause/resume/kill)"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("'command' is required"),
        };

        let guard = self.registry.read().await;
        let registry = match guard.as_ref() {
            Some(r) => r,
            None => return ToolResult::error("Subagent registry not initialized"),
        };

        match command {
            "list" => {
                let agents = registry.list_all();
                if agents.is_empty() {
                    ToolResult::text("No subagents running.")
                } else {
                    let output: Vec<String> = agents.iter().map(|a| a.to_string()).collect();
                    ToolResult::text(output.join("\n"))
                }
            }

            "query" | "pause" | "resume" | "kill" => {
                let name_input = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => {
                        return ToolResult::error(format!("'name' is required for '{}'", command))
                    }
                };

                // Try exact match first, then prefix
                let resolved_name = if registry.query(name_input).is_ok() {
                    name_input.to_string()
                } else {
                    match registry.find_by_prefix(name_input) {
                        Some(n) => n,
                        None => {
                            return ToolResult::error(format!(
                                "Subagent '{}' not found",
                                name_input
                            ))
                        }
                    }
                };

                match command {
                    "query" => match registry.query(&resolved_name) {
                        Ok(info) => ToolResult::text(info.to_string()),
                        Err(e) => ToolResult::error(e),
                    },
                    "pause" => match registry.pause(&resolved_name) {
                        Ok(()) => ToolResult::text(format!("Paused: {}", resolved_name)),
                        Err(e) => ToolResult::error(e),
                    },
                    "resume" => match registry.resume(&resolved_name) {
                        Ok(()) => ToolResult::text(format!("Resumed: {}", resolved_name)),
                        Err(e) => ToolResult::error(e),
                    },
                    "kill" => match registry.kill(&resolved_name) {
                        Ok(()) => ToolResult::text(format!("Killed: {}", resolved_name)),
                        Err(e) => ToolResult::error(e),
                    },
                    _ => unreachable!(),
                }
            }

            _ => ToolResult::error(format!("Unknown command: {}", command)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spawn::SubagentInfoSnapshot;

    /// Mock SubagentControl for testing the tool
    struct MockRegistry {
        agents: std::sync::Mutex<Vec<SubagentInfoSnapshot>>,
    }

    impl MockRegistry {
        fn new() -> Self {
            Self {
                agents: std::sync::Mutex::new(vec![SubagentInfoSnapshot {
                    name: "fix-bug-abc12".to_string(),
                    purpose: "Fix the bug".to_string(),
                    status: "running".to_string(),
                    tokens: 100,
                    elapsed_ms: 5000,
                    last_output: String::new(),
                    children: vec![],
                    parent: None,
                }]),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::tools::spawn::SubagentControl for MockRegistry {
        fn register(&self, purpose: &str, _session_id: &str, _parent: Option<&str>) -> String {
            format!("mock-{}", purpose.replace(' ', "-").to_lowercase())
        }
        fn mark_complete(&self, _name: &str, _output: &str, _tokens: usize) {}
        fn mark_failed(&self, _name: &str, _error: &str) {}
        fn is_paused(&self, _name: &str) -> bool {
            false
        }
        fn is_killed(&self, _name: &str) -> bool {
            false
        }
        async fn wait_for_resume(&self, _name: &str) {}
        fn pause(&self, name: &str) -> Result<(), String> {
            if name == "fix-bug-abc12" {
                Ok(())
            } else {
                Err("not found".into())
            }
        }
        fn resume(&self, name: &str) -> Result<(), String> {
            if name == "fix-bug-abc12" {
                Ok(())
            } else {
                Err("not found".into())
            }
        }
        fn kill(&self, name: &str) -> Result<(), String> {
            if name == "fix-bug-abc12" {
                Ok(())
            } else {
                Err("not found".into())
            }
        }
        fn query(&self, name: &str) -> Result<SubagentInfoSnapshot, String> {
            let agents = self.agents.lock().unwrap();
            agents
                .iter()
                .find(|a| a.name == name)
                .cloned()
                .ok_or_else(|| "not found".into())
        }
        fn list_all(&self) -> Vec<SubagentInfoSnapshot> {
            self.agents.lock().unwrap().clone()
        }
        fn find_by_prefix(&self, prefix: &str) -> Option<String> {
            let agents = self.agents.lock().unwrap();
            agents
                .iter()
                .find(|a| a.name.starts_with(prefix))
                .map(|a| a.name.clone())
        }
    }

    #[tokio::test]
    async fn test_list_command() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool.execute(json!({"command": "list"})).await;
        let text = result.to_content_string();
        assert!(text.contains("fix-bug-abc12"));
        assert!(text.contains("running"));
    }

    #[tokio::test]
    async fn test_query_command() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool
            .execute(json!({"command": "query", "name": "fix-bug-abc12"}))
            .await;
        assert!(!result.is_error());
        assert!(result.to_content_string().contains("Fix the bug"));
    }

    #[tokio::test]
    async fn test_pause_command() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool
            .execute(json!({"command": "pause", "name": "fix-bug-abc12"}))
            .await;
        assert!(!result.is_error());
        assert!(result.to_content_string().contains("Paused"));
    }

    #[tokio::test]
    async fn test_kill_command() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool
            .execute(json!({"command": "kill", "name": "fix-bug-abc12"}))
            .await;
        assert!(!result.is_error());
        assert!(result.to_content_string().contains("Killed"));
    }

    #[tokio::test]
    async fn test_missing_name() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool.execute(json!({"command": "pause"})).await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_not_found() {
        let mock: SubagentRegistryHandle = Arc::new(MockRegistry::new());
        let handle = Arc::new(RwLock::new(Some(mock)));
        let tool = SubagentTool::new(handle);

        let result = tool
            .execute(json!({"command": "query", "name": "nonexistent"}))
            .await;
        assert!(result.is_error());
    }
}
