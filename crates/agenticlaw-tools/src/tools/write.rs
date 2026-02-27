//! Write tool — create or overwrite a file

use crate::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

pub struct WriteTool {
    workspace_root: PathBuf,
}

impl WriteTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if needed. \
         Overwrites the file if it exists. Prefer edit for modifications."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["file_path", "content"]
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
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: content"),
        };

        let full_path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        if let Some(parent) = full_path.parent() {
            if let Err(e) = fs::create_dir_all(parent).await {
                return ToolResult::error(format!("Failed to create directories: {}", e));
            }
        }

        match fs::write(&full_path, content).await {
            Ok(()) => {
                debug!("write: {} ({} bytes)", path, content.len());
                ToolResult::text(format!("Wrote {} bytes to {}", content.len(), path))
            }
            Err(e) => ToolResult::error(format!("Failed to write: {}", e)),
        }
    }
}
