//! Grep tool — content search with regex support

use crate::registry::{Tool, ToolResult};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing::debug;
use walkdir::WalkDir;

pub struct GrepTool {
    workspace_root: PathBuf,
}

impl GrepTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regex patterns. Returns matching file paths by default, \
         or matching lines with context. Use glob parameter to filter files."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search (default: workspace root)"
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs', '*.{ts,tsx}')"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "Output mode (default: files_with_matches)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search (default: false)"
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context around matches (for content mode)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let pattern_str = match args["pattern"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
        let regex_pattern = if case_insensitive {
            format!("(?i){}", pattern_str)
        } else {
            pattern_str.to_string()
        };

        let regex = match Regex::new(&regex_pattern) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid regex: {}", e)),
        };

        let search_root = args["path"]
            .as_str()
            .map(|p| {
                if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    self.workspace_root.join(p)
                }
            })
            .unwrap_or_else(|| self.workspace_root.clone());

        let output_mode = args["output_mode"].as_str().unwrap_or("files_with_matches");
        let context_lines = args["context"].as_u64().unwrap_or(0) as usize;

        let file_glob = args["glob"].as_str().and_then(|g| {
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .ok()
                .map(|g| g.compile_matcher())
        });

        // If searching a single file
        if search_root.is_file() {
            return search_file(&search_root, &regex, output_mode, context_lines);
        }

        let mut results = Vec::new();
        let mut _match_count = 0;

        for entry in WalkDir::new(&search_root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && name != "node_modules" && name != "target"
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            // Apply glob filter
            if let Some(ref glob) = file_glob {
                let name = entry.file_name().to_string_lossy();
                if !glob.is_match(name.as_ref()) {
                    continue;
                }
            }

            // Skip binary files (check first 512 bytes)
            if let Ok(bytes) = std::fs::read(entry.path()) {
                if bytes.len() > 512 && bytes[..512].contains(&0) {
                    continue;
                }
            } else {
                continue;
            }

            let content = match std::fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if regex.is_match(&content) {
                match output_mode {
                    "files_with_matches" => {
                        results.push(entry.path().to_string_lossy().to_string());
                    }
                    "count" => {
                        let count = regex.find_iter(&content).count();
                        results.push(format!("{}:{}", entry.path().display(), count));
                        _match_count += count;
                    }
                    "content" => {
                        let lines: Vec<&str> = content.lines().collect();
                        for (i, line) in lines.iter().enumerate() {
                            if regex.is_match(line) {
                                let start = i.saturating_sub(context_lines);
                                let end = (i + context_lines + 1).min(lines.len());
                                for j in start..end {
                                    let prefix = if j == i { ">" } else { " " };
                                    results.push(format!(
                                        "{}{}:{}:{}",
                                        prefix,
                                        entry.path().display(),
                                        j + 1,
                                        lines[j]
                                    ));
                                }
                                if context_lines > 0 && end < lines.len() {
                                    results.push("--".to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            if results.len() > 5000 {
                break;
            } // cap output
        }

        debug!("grep: '{}' → {} results", pattern_str, results.len());

        if results.is_empty() {
            ToolResult::text("No matches found")
        } else {
            ToolResult::text(results.join("\n"))
        }
    }
}

fn search_file(path: &Path, regex: &Regex, output_mode: &str, context_lines: usize) -> ToolResult {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read: {}", e)),
    };

    if !regex.is_match(&content) {
        return ToolResult::text("No matches found");
    }

    match output_mode {
        "files_with_matches" => ToolResult::text(path.to_string_lossy().to_string()),
        "count" => ToolResult::text(format!("{}", regex.find_iter(&content).count())),
        "content" | _ => {
            let lines: Vec<&str> = content.lines().collect();
            let mut results = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                if regex.is_match(line) {
                    let start = i.saturating_sub(context_lines);
                    let end = (i + context_lines + 1).min(lines.len());
                    for j in start..end {
                        let prefix = if j == i { ">" } else { " " };
                        results.push(format!("{}{}:{}", prefix, j + 1, lines[j]));
                    }
                    if context_lines > 0 {
                        results.push("--".to_string());
                    }
                }
            }
            ToolResult::text(results.join("\n"))
        }
    }
}
