//! Read tool — read file contents with optional offset/limit

use crate::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

pub struct ReadTool {
    workspace_root: PathBuf,
}

impl ReadTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, String> {
        let p = Path::new(path);
        let expanded = if path.starts_with("~/") {
            dirs::home_dir()
                .unwrap_or_default()
                .join(path.strip_prefix("~/").unwrap())
        } else if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace_root.join(p)
        };
        // Resolve symlinks if the file exists, otherwise use the path as-is
        let resolved = expanded.canonicalize().unwrap_or(expanded);
        Ok(resolved)
    }
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns numbered lines. Use offset/limit for large files."
    }

    fn prompt(&self) -> &str {
        "Use the read tool to view files. Read files before editing them."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or workspace-relative path to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default 2000)"
                }
            },
            "required": ["file_path"]
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

        let resolved = match self.resolve_path(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        let content = match fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        let offset = args["offset"].as_u64().unwrap_or(1) as usize;
        let limit = args["limit"].as_u64().unwrap_or(2000) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let start = (offset.saturating_sub(1)).min(lines.len());
        let end = (start + limit).min(lines.len());

        // Number lines like cat -n
        let result: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect();

        debug!(
            "read: {} ({} lines from offset {})",
            path,
            end - start,
            offset
        );
        ToolResult::text(result.join("\n"))
    }
}
