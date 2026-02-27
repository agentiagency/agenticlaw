//! Edit tool — find and replace exact strings in files

use crate::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

pub struct EditTool {
    workspace_root: PathBuf,
}

impl EditTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string match. The old_string must appear \
         exactly once in the file. Use replace_all to replace all occurrences."
    }

    fn prompt(&self) -> &str {
        "Always read a file before editing it. The old_string must match exactly \
         including whitespace and indentation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let path = match args
            .get("file_path")
            .or(args.get("path"))
            .and_then(|v| v.as_str())
        {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: file_path"),
        };
        let old = match args["old_string"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: old_string"),
        };
        let new = match args["new_string"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: new_string"),
        };
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        let full_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        let content = match fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        if !content.contains(old) {
            return ToolResult::error("old_string not found in file");
        }

        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            let count = content.matches(old).count();
            if count > 1 {
                return ToolResult::error(format!(
                    "old_string found {} times — must be unique. Use replace_all or provide more context.",
                    count
                ));
            }
            content.replacen(old, new, 1)
        };

        match fs::write(&full_path, &new_content).await {
            Ok(()) => {
                debug!("edit: {}", path);
                ToolResult::text(format!("Edited {}", path))
            }
            Err(e) => ToolResult::error(format!("Failed to write: {}", e)),
        }
    }
}
