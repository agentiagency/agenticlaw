//! Spawn tool — the KG primitive. Any agent can spawn a child agent with FEAR/EGO/PURPOSE.
//!
//! This is not a utility — it IS the knowledge graph executor. When an agent calls spawn,
//! it descends the graph: code prepares the child's context, the child reasons within
//! boundaries, code captures the result and returns it to the parent.
//!
//! The observability layer (resource driver) records everything structurally:
//! prompt, fear, ego, transcript, output, metrics — all written by code, not the agent.

use crate::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared handle to the agent runtime, set after construction.
/// This breaks the circular dependency: tools need runtime, runtime needs tools.
pub type RuntimeHandle = Arc<RwLock<Option<Arc<dyn SpawnableRuntime>>>>;

/// Shared handle to the subagent registry for lifecycle management.
pub type SubagentRegistryHandle = Arc<dyn SubagentControl>;

/// Trait for subagent lifecycle control — implemented by SubagentRegistry.
/// Decouples the tool layer from the agent layer.
#[async_trait::async_trait]
pub trait SubagentControl: Send + Sync {
    /// Register a new subagent, returns its purpose-hash name.
    fn register(&self, purpose: &str, session_id: &str, parent: Option<&str>) -> String;
    /// Mark complete with output and tokens.
    fn mark_complete(&self, name: &str, output: &str, tokens: usize);
    /// Mark failed.
    fn mark_failed(&self, name: &str, error: &str);
    /// Check if paused.
    fn is_paused(&self, name: &str) -> bool;
    /// Check if killed.
    fn is_killed(&self, name: &str) -> bool;
    /// Wait on the pause gate (blocks until resumed or killed).
    async fn wait_for_resume(&self, name: &str);
    /// Pause a subagent (recursive).
    fn pause(&self, name: &str) -> Result<(), String>;
    /// Resume a subagent (recursive).
    fn resume(&self, name: &str) -> Result<(), String>;
    /// Kill a subagent (recursive).
    fn kill(&self, name: &str) -> Result<(), String>;
    /// Query subagent info.
    fn query(&self, name: &str) -> Result<SubagentInfoSnapshot, String>;
    /// List all subagents.
    fn list_all(&self) -> Vec<SubagentInfoSnapshot>;
    /// Find by prefix.
    fn find_by_prefix(&self, prefix: &str) -> Option<String>;
}

/// Snapshot of subagent info (decoupled from agent crate types).
#[derive(Debug, Clone)]
pub struct SubagentInfoSnapshot {
    pub name: String,
    pub purpose: String,
    pub status: String,
    pub tokens: usize,
    pub elapsed_ms: u64,
    pub last_output: String,
    pub children: Vec<String>,
    pub parent: Option<String>,
}

impl std::fmt::Display for SubagentInfoSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}] — {} ({}ms, ~{}tok)",
            self.name, self.status, self.purpose, self.elapsed_ms, self.tokens
        )?;
        if !self.last_output.is_empty() {
            let preview = if self.last_output.len() > 100 {
                format!("{}...", &self.last_output[..97])
            } else {
                self.last_output.clone()
            };
            write!(f, "\n  └─ {}", preview)?;
        }
        Ok(())
    }
}

/// Trait that the agent runtime implements to support spawning.
/// Decouples the tool from the concrete runtime type.
#[async_trait::async_trait]
pub trait SpawnableRuntime: Send + Sync {
    /// Run a child agent turn with the given system prompt and user message.
    /// Returns (output_text, token_estimate).
    async fn spawn_child(
        &self,
        session_id: &str,
        system_prompt: &str,
        user_message: &str,
        max_iterations: usize,
    ) -> Result<(String, usize), String>;
}

pub struct SpawnTool {
    #[allow(dead_code)]
    workspace_root: PathBuf,
    runtime: RuntimeHandle,
    /// Directory for run artifacts. If None, observability is disabled.
    runs_dir: Option<PathBuf>,
    /// Counter for generating unique child IDs within a session.
    child_counter: Arc<std::sync::atomic::AtomicU64>,
    /// Subagent registry for lifecycle tracking.
    subagent_registry: Option<Arc<RwLock<Option<SubagentRegistryHandle>>>>,
}

impl SpawnTool {
    pub fn new(workspace_root: impl AsRef<Path>, runtime: RuntimeHandle) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            runtime,
            runs_dir: dirs::home_dir().map(|h| h.join("tmp/kg-runs")),
            child_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            subagent_registry: None,
        }
    }

    pub fn with_runs_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.runs_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn with_subagent_registry(
        mut self,
        registry: Arc<RwLock<Option<SubagentRegistryHandle>>>,
    ) -> Self {
        self.subagent_registry = Some(registry);
        self
    }

    fn next_child_id(&self) -> u64 {
        self.child_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Write an artifact to the run directory (code, not agent).
    async fn write_artifact(&self, run_dir: &Path, name: &str, content: &str) {
        if let Err(e) = tokio::fs::create_dir_all(run_dir).await {
            tracing::warn!("failed to create run dir: {}", e);
            return;
        }
        if let Err(e) = tokio::fs::write(run_dir.join(name), content).await {
            tracing::warn!("failed to write artifact {}: {}", name, e);
        }
    }
}

#[async_trait::async_trait]
impl Tool for SpawnTool {
    fn name(&self) -> &str {
        "spawn"
    }

    fn description(&self) -> &str {
        "Spawn a child agent to perform a scoped task. The child has the full tool suite \
         and discovers its own context by reading files. You just provide the purpose and task. \
         The child orients itself — you don't need to pass it context (that wastes your tokens)."
    }

    fn prompt(&self) -> &str {
        "The spawn tool creates a child agent. Keep it simple:\n\
         - PURPOSE: one sentence — what should this child accomplish?\n\
         - TASK: what to do — be specific about location and goal\n\
         - The child has bash, read, write, edit, grep, glob, spawn — it can read files itself\n\
         - Do NOT paste file contents into ego — the child reads its own files (cheaper)\n\
         - Use fear only when you need hard constraints (e.g. 'do not modify tests')\n\
         - Children can spawn grandchildren — recursive decomposition is natural\n\n\
         Good: spawn(purpose='Fix slider CSS', task='Read issue #183, find slider components in web/src, fix styling')\n\
         Bad: spawn(purpose='Fix slider', ego='<500 lines of file contents>', task='fix it')"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["purpose", "task"],
            "properties": {
                "purpose": {
                    "type": "string",
                    "description": "Why this child exists. One sentence. Falsifiable goal."
                },
                "task": {
                    "type": "string",
                    "description": "What to do. Be specific about location and goal. The child reads its own files."
                },
                "fear": {
                    "type": "string",
                    "description": "Optional hard constraints. Only use when needed (e.g. 'do not modify CI/CD', 'max 10 file changes'). Omit for defaults."
                },
                "ego": {
                    "type": "string",
                    "description": "Optional pre-loaded context. Usually unnecessary — the child reads files itself. Only use for passing approved plans or decisions from a prior phase."
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Max tool call iterations (default 25, max 50)"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let purpose = args
            .get("purpose")
            .and_then(|v| v.as_str())
            .unwrap_or("unspecified");
        let ego = args.get("ego").and_then(|v| v.as_str()).unwrap_or("");
        let fear = args.get("fear").and_then(|v| v.as_str()).unwrap_or("");
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("'task' is required"),
        };
        let max_iter = args
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .unwrap_or(25)
            .min(50) as usize;

        let child_id = self.next_child_id();
        let session_id = format!(
            "kg-child-{}-{}",
            child_id,
            chrono::Utc::now().format("%H%M%S")
        );

        // Register with subagent registry for lifecycle tracking
        let subagent_name = if let Some(ref reg_handle) = self.subagent_registry {
            let guard = reg_handle.read().await;
            guard
                .as_ref()
                .map(|reg| reg.register(purpose, &session_id, None))
        } else {
            None
        };

        tracing::info!(
            child = %session_id,
            purpose = %purpose,
            "spawning child agent"
        );

        // --- CODE: Write artifacts BEFORE spawn ---
        let run_dir = self
            .runs_dir
            .as_ref()
            .map(|d| d.join(format!("child-{}", session_id)));

        if let Some(ref dir) = run_dir {
            self.write_artifact(dir, "purpose.md", purpose).await;
            self.write_artifact(dir, "ego.md", ego).await;
            self.write_artifact(dir, "fear.md", fear).await;
            self.write_artifact(dir, "prompt.md", task).await;
            self.write_artifact(
                dir,
                "manifest.yaml",
                &format!(
                    "child_id: {}\npurpose: {:?}\nstarted: {}\nstatus: running\n",
                    session_id,
                    purpose,
                    chrono::Utc::now().to_rfc3339()
                ),
            )
            .await;
        }

        // --- CODE: Build system prompt from FEAR/EGO/PURPOSE ---
        // Lean by default. Child discovers its own context.
        let mut system_parts = vec![format!("PURPOSE: {purpose}")];

        if !fear.is_empty() {
            system_parts.push(format!("CONSTRAINTS:\n{fear}"));
        }

        if !ego.is_empty() {
            system_parts.push(format!("CONTEXT:\n{ego}"));
        }

        system_parts.push(
            "You are a focused agent. Read the files you need. Execute precisely. \
             Report what you did and what changed."
                .into(),
        );

        let system_prompt = system_parts.join("\n\n");

        let start = std::time::Instant::now();

        // --- AGENTIC: Spawn the child ---
        let runtime_guard = self.runtime.read().await;
        let runtime = match runtime_guard.as_ref() {
            Some(r) => r.clone(),
            None => {
                return ToolResult::error(
                    "Runtime not initialized — spawn tool cannot create child agents",
                );
            }
        };
        drop(runtime_guard); // release lock before async work

        let result = runtime
            .spawn_child(&session_id, &system_prompt, task, max_iter)
            .await;
        let wall_ms = start.elapsed().as_millis() as u64;

        // --- CODE: Write results AFTER spawn ---
        match &result {
            Ok((output, tokens)) => {
                tracing::info!(
                    child = %session_id,
                    tokens = tokens,
                    wall_ms = wall_ms,
                    "child completed successfully"
                );

                // Update subagent registry
                if let (Some(ref name), Some(ref reg_handle)) =
                    (&subagent_name, &self.subagent_registry)
                {
                    let guard = reg_handle.read().await;
                    if let Some(ref reg) = *guard {
                        reg.mark_complete(name, output, *tokens);
                    }
                }

                if let Some(ref dir) = run_dir {
                    self.write_artifact(dir, "output.md", output).await;
                    self.write_artifact(
                        dir,
                        "metrics.yaml",
                        &format!(
                            "tokens: {}\nwall_ms: {}\noutcome: success\n",
                            tokens, wall_ms
                        ),
                    )
                    .await;
                    // Update manifest
                    self.write_artifact(dir, "manifest.yaml", &format!(
                        "child_id: {}\npurpose: {:?}\nstarted: {}\nended: {}\nstatus: success\ntokens: {}\nwall_ms: {}\n",
                        session_id, purpose, chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339(), tokens, wall_ms
                    )).await;
                }

                let name_info = subagent_name.as_deref().unwrap_or(&session_id);
                ToolResult::text(format!("[{}] {}", name_info, output))
            }
            Err(e) => {
                tracing::warn!(
                    child = %session_id,
                    error = %e,
                    wall_ms = wall_ms,
                    "child failed"
                );

                // Update subagent registry
                if let (Some(ref name), Some(ref reg_handle)) =
                    (&subagent_name, &self.subagent_registry)
                {
                    let guard = reg_handle.read().await;
                    if let Some(ref reg) = *guard {
                        reg.mark_failed(name, &e.to_string());
                    }
                }

                if let Some(ref dir) = run_dir {
                    self.write_artifact(dir, "output.md", &format!("ERROR: {}", e))
                        .await;
                    self.write_artifact(
                        dir,
                        "metrics.yaml",
                        &format!(
                            "tokens: 0\nwall_ms: {}\noutcome: failed\nerror: {:?}\n",
                            wall_ms, e
                        ),
                    )
                    .await;
                }

                ToolResult::error(format!("Child agent failed: {}", e))
            }
        }
    }
}
