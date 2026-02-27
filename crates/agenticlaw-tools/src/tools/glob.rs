//! Glob tool — fast file pattern matching

use crate::registry::{Tool, ToolResult};
use globset::GlobBuilder;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing::debug;
use walkdir::WalkDir;

pub struct GlobTool {
    workspace_root: PathBuf,
}

impl GlobTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self { workspace_root: workspace_root.as_ref().to_path_buf() }
    }
}

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Supports ** for recursive matching. \
         Returns file paths sorted by modification time (newest first)."
    }

    fn is_read_only(&self) -> bool { true }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g. '**/*.rs', 'src/**/*.ts')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: workspace root)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let pattern = match args["pattern"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let search_root = args["path"].as_str()
            .map(|p| if Path::new(p).is_absolute() { PathBuf::from(p) } else { self.workspace_root.join(p) })
            .unwrap_or_else(|| self.workspace_root.clone());

        let glob = match GlobBuilder::new(pattern).literal_separator(false).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolResult::error(format!("Invalid glob pattern: {}", e)),
        };

        let mut matches: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

        for entry in WalkDir::new(&search_root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && name != "node_modules" && name != "target"
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rel_path = entry.path().strip_prefix(&search_root)
                    .unwrap_or(entry.path());
                if glob.is_match(rel_path) {
                    let mtime = entry.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    matches.push((entry.path().to_path_buf(), mtime));
                }
            }
        }

        // Sort by modification time, newest first
        matches.sort_by(|a, b| b.1.cmp(&a.1));

        debug!("glob: '{}' → {} matches", pattern, matches.len());

        if matches.is_empty() {
            ToolResult::text("No files found")
        } else {
            let result: Vec<String> = matches.iter()
                .take(1000) // cap output
                .map(|(p, _)| p.to_string_lossy().to_string())
                .collect();
            ToolResult::text(result.join("\n"))
        }
    }
}
