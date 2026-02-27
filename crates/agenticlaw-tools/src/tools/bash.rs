//! Bash tool — execute shell commands with timeout, background support, and cancellation

use crate::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub struct BashTool {
    workspace_root: PathBuf,
    default_timeout_secs: u64,
}

impl BashTool {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            default_timeout_secs: 120,
        }
    }
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

    fn description(&self) -> &str {
        "Execute a bash command. Use for git, npm, docker, system commands. \
         Captures stdout and stderr. Set timeout in seconds (default 120). \
         Include a short description of what the command does."
    }

    fn prompt(&self) -> &str {
        "Use the bash tool for terminal operations. Quote paths with spaces. \
         Prefer dedicated tools (read, write, edit, glob, grep) over bash equivalents."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 120, max 600)"
                },
                "description": {
                    "type": "string",
                    "description": "Short description of what this command does"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let command = match args["command"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: command"),
        };

        let timeout_secs = args["timeout"].as_u64()
            .unwrap_or(self.default_timeout_secs)
            .min(600);

        if let Some(desc) = args["description"].as_str() {
            debug!("bash [{}]: {}", desc, command);
        } else {
            debug!("bash: {}", &command[..command.len().min(80)]);
        }

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new("bash")
                .arg("-c")
                .arg(command)
                .current_dir(&self.workspace_root)
                .output()
        ).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return ToolResult::error(format!("Failed to execute: {}", e)),
            Err(_) => return ToolResult::error(format!("Command timed out after {}s", timeout_secs)),
        };

        format_output(&output)
    }

    /// Cancellable execution: spawns the process with kill_on_drop(true) and
    /// races against the CancellationToken. On cancellation, the child process
    /// is killed immediately.
    async fn execute_cancellable(
        &self,
        args: Value,
        cancel: CancellationToken,
    ) -> ToolResult {
        let command = match args["command"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: command"),
        };

        let timeout_secs = args["timeout"].as_u64()
            .unwrap_or(self.default_timeout_secs)
            .min(600);

        if let Some(desc) = args["description"].as_str() {
            debug!("bash (cancellable) [{}]: {}", desc, command);
        } else {
            debug!("bash (cancellable): {}", &command[..command.len().min(80)]);
        }

        let mut child = match Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace_root)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return ToolResult::error(format!("Failed to spawn: {}", e)),
        };

        // Race: wait for the process vs cancellation vs timeout.
        // We use wait() + manual stdout/stderr reading instead of wait_with_output()
        // because wait_with_output() takes ownership and prevents kill-on-cancel.
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        tokio::select! {
            result = async {
                tokio::time::timeout(timeout_duration, child.wait()).await
            } => {
                match result {
                    Ok(Ok(status)) => {
                        // Process exited — read stdout/stderr
                        let stdout = read_pipe(child.stdout.take()).await;
                        let stderr = read_pipe(child.stderr.take()).await;
                        let output = std::process::Output {
                            status,
                            stdout: stdout.into_bytes(),
                            stderr: stderr.into_bytes(),
                        };
                        format_output(&output)
                    }
                    Ok(Err(e)) => ToolResult::error(format!("Failed to wait: {}", e)),
                    Err(_) => {
                        // Timeout — kill the process
                        let _ = child.kill().await;
                        ToolResult::error(format!("Command timed out after {}s", timeout_secs))
                    }
                }
            }
            _ = cancel.cancelled() => {
                // Interrupted by human — kill the process immediately
                let _ = child.kill().await;
                ToolResult::text("[process killed by interrupt]")
            }
        }
    }
}

/// Read all bytes from an optional child pipe into a string.
async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    use tokio::io::AsyncReadExt;
    match pipe {
        Some(mut p) => {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).to_string()
        }
        None => String::new(),
    }
}

fn format_output(output: &std::process::Output) -> ToolResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let result = if output.status.success() {
        if stderr.is_empty() {
            stdout.trim().to_string()
        } else {
            format!("{}\n{}", stdout.trim(), stderr.trim())
        }
    } else {
        format!("Exit code: {}\n{}\n{}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        )
    };

    if result.is_empty() {
        ToolResult::text("(no output)")
    } else if result.len() > 30000 {
        ToolResult::text(format!(
            "{}\n... [truncated, {} total chars]",
            &result[..30000],
            result.len()
        ))
    } else {
        ToolResult::text(result)
    }
}
