//! KG Executor — graph descent algorithm with agentic observation hooks.
//!
//! This is a combination of HITL, agent, and code at every layer:
//! - CODE: graph traversal, resource management, FEAR/EGO/PURPOSE preparation
//! - AGENT: reasoning at observation points within code-set boundaries
//! - HITL: gates, approval, steering (via RunConfig and iteration)
//!
//! The executor DESCENDS the node type registry recursively:
//! - Parent nodes: prepare context from children's outputs, aggregate results
//! - Leaf nodes: spawn an agent with CODE-prepared FEAR/EGO/PURPOSE, capture output
//!
//! Every agent instantiation is preceded by deterministic preparation of:
//! - PURPOSE: why this node exists (one sentence, falsifiable)
//! - EGO: the node's view of the world (exactly the context it needs)
//! - FEAR: constraints, boundaries, what goes wrong if you deviate

use crate::manifest::{NodeState, Outcome, RunManifest};
use crate::registry::{NodeType, NodeTypeRegistry, TemplateVars};
use crate::resource::{Artifact, GraphAddress, KgEvent, ResourceDriver};
use agenticlaw_agent::runtime::{AgentEvent, AgentRuntime};
use agenticlaw_agent::session::SessionKey;
use anyhow::Result;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Simple hash for logging (not cryptographic).
#[allow(dead_code)]
fn short_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// The FEAR/EGO/PURPOSE triad — prepared by CODE for every agent instantiation.
#[derive(Clone, Debug)]
pub struct NodePrep {
    /// PURPOSE: why this node exists. One sentence. Falsifiable.
    pub purpose: String,
    /// EGO: the node's view of the world. Exactly the context it needs, no more.
    pub ego: String,
    /// FEAR: constraints, boundaries. What goes wrong if you deviate.
    pub fear: String,
}

/// Configuration for a single KG execution run.
pub struct RunConfig {
    /// Human-readable purpose ("Fix slider styling regression")
    pub purpose: String,
    /// Target resource ("github.com/agentiagency/agentimolt-v03/issues/183")
    pub target: String,
    /// Issue number (used for graph addressing)
    pub issue: u64,
    /// System prompt for agents
    pub system_prompt: String,
    /// Analysis prompt — sent to the analysis child (LEGACY, used if no registry)
    pub analysis_prompt: String,
    /// Context files the root read to scope this run
    pub context_summary: String,
    /// Max iterations per phase before giving up
    pub max_iterations: usize,
}

/// Result of a completed node (leaf or parent).
#[derive(Clone, Debug)]
pub struct NodeResult {
    pub output: String,
    pub tokens: usize,
    pub wall_ms: u64,
    pub success: bool,
    /// For parents: concatenated child outputs.
    pub child_outputs: Vec<(String, String)>, // (node_id, output)
}

/// The KG Executor. Drives recursive agent tree with structural observability.
pub struct Executor {
    runtime: Arc<AgentRuntime>,
    driver: Arc<dyn ResourceDriver>,
    registry: NodeTypeRegistry,
}

impl Executor {
    pub fn new(runtime: Arc<AgentRuntime>, driver: Arc<dyn ResourceDriver>) -> Self {
        Self {
            runtime,
            driver,
            registry: crate::registry::default_issue_registry(),
        }
    }

    pub fn with_registry(
        runtime: Arc<AgentRuntime>,
        driver: Arc<dyn ResourceDriver>,
        registry: NodeTypeRegistry,
    ) -> Self {
        Self {
            runtime,
            driver,
            registry,
        }
    }

    /// Execute a full issue run using the node type registry.
    /// Recursively descends: issue → analysis/impl/pr → leaves.
    pub async fn run_issue(&self, config: RunConfig) -> Result<RunManifest> {
        let run_id = format!("{}-{}", config.issue, Utc::now().format("%Y%m%dT%H%M%S"));
        let graph_root = GraphAddress::root()
            .child("issue")
            .child(&config.issue.to_string());

        // Build template variables from config
        let mut vars = TemplateVars {
            issue: config.issue.to_string(),
            repo: config
                .target
                .split('/')
                .take(2)
                .collect::<Vec<_>>()
                .join("/"),
            context: config.context_summary.clone(),
            ..Default::default()
        };
        // Try to extract repo from target (github.com/owner/repo/issues/N → owner/repo)
        if config.target.contains("github.com/") {
            let parts: Vec<&str> = config
                .target
                .trim_start_matches("github.com/")
                .split('/')
                .collect();
            if parts.len() >= 2 {
                vars.repo = format!("{}/{}", parts[0], parts[1]);
            }
        }

        // Initialize manifest with all nodes from registry (recursive)
        let mut manifest = RunManifest::new(
            &run_id,
            &config.purpose,
            graph_root.as_str(),
            &config.target,
        );
        self.register_nodes_recursive("issue", &graph_root, &mut manifest);

        // Write initial manifest
        self.write_manifest(&run_id, &manifest).await?;

        // Write root context
        self.driver
            .write_artifact(
                &run_id,
                &GraphAddress::root(),
                Artifact::Context,
                config.context_summary.as_bytes(),
            )
            .await?;

        // --- RECURSIVE DESCENT ---
        let result = self
            .execute_node(
                &run_id,
                "issue",
                &graph_root,
                &config.system_prompt,
                &vars,
                &mut manifest,
            )
            .await?;

        // Finalize
        let outcome = if result.success {
            Outcome::Success
        } else {
            Outcome::Failure
        };
        manifest.finalize(outcome);
        self.write_manifest(&run_id, &manifest).await?;

        // Write final report
        let report = self.build_report(&run_id, &manifest, &result);
        self.write_report(&run_id, &manifest, &report).await?;

        info!(run_id, outcome = %manifest.outcome, tokens = manifest.total_tokens, "run complete");
        Ok(manifest)
    }

    /// Recursively register all nodes in the manifest.
    fn register_nodes_recursive(
        &self,
        node_type_id: &str,
        addr: &GraphAddress,
        manifest: &mut RunManifest,
    ) {
        let initial_state = if node_type_id == "issue" {
            NodeState::Pending
        } else {
            NodeState::Blocked
        };
        manifest.add_node(addr.as_str(), initial_state);

        if let Some(nt) = self.registry.get(node_type_id) {
            for child_id in &nt.children {
                let child_addr = addr.child(child_id);
                self.register_nodes_recursive(child_id, &child_addr, manifest);
            }
        }
    }

    /// Execute a node: if leaf, spawn agent; if parent, descend into children.
    fn execute_node<'a>(
        &'a self,
        run_id: &'a str,
        node_type_id: &'a str,
        addr: &'a GraphAddress,
        system_prompt: &'a str,
        vars: &'a TemplateVars,
        manifest: &'a mut RunManifest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<NodeResult>> + Send + 'a>> {
        Box::pin(async move {
            let node_type = self
                .registry
                .get(node_type_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("node type '{}' not found in registry", node_type_id)
                })?
                .clone(); // Clone to avoid borrow issues

            info!(
                run_id,
                node = node_type_id,
                addr = addr.as_str(),
                leaf = node_type.is_leaf,
                "executing node"
            );

            // Mark as running
            manifest.start_node(addr.as_str(), None);
            self.write_manifest(run_id, manifest).await?;

            if node_type.is_leaf {
                self.execute_leaf(run_id, &node_type, addr, system_prompt, vars, manifest)
                    .await
            } else {
                self.execute_parent(run_id, &node_type, addr, system_prompt, vars, manifest)
                    .await
            }
        }) // close Box::pin
    }

    /// Execute a leaf node: spawn an agent with CODE-prepared FEAR/EGO/PURPOSE.
    async fn execute_leaf(
        &self,
        run_id: &str,
        node_type: &NodeType,
        addr: &GraphAddress,
        _system_prompt: &str,
        vars: &TemplateVars,
        manifest: &mut RunManifest,
    ) -> Result<NodeResult> {
        // ── CODE prepares everything. Agent receives it preloaded. ──

        let purpose = crate::registry::render_template_pub(&node_type.purpose_template, vars);
        let node_fear = crate::registry::render_template_pub(&node_type.fear_template, vars);
        let prompt = crate::registry::render_template_pub(&node_type.prompt_template, vars);
        // ROOT FEAR is inherited by every node — prepended, non-negotiable
        let fear = format!(
            "{}\n\n---\n\n{}",
            crate::registry::AGENTIMOLT_ROOT_FEAR,
            node_fear
        );
        let ego = vars.parent_output.clone(); // EGO = what the parent prepared

        // Write the directory — this IS the agent's brain at spawn time
        self.driver
            .write_artifact(run_id, addr, Artifact::Prompt, prompt.as_bytes())
            .await?;
        self.driver
            .write_artifact(run_id, addr, Artifact::Ego, ego.as_bytes())
            .await?;
        self.driver
            .write_artifact(run_id, addr, Artifact::Fear, fear.as_bytes())
            .await?;

        self.emit_event(
            run_id,
            addr,
            "leaf_spawned",
            serde_json::json!({
                "node_type": node_type.id,
                "purpose": purpose,
                "role": node_type.role.to_string(),
                "max_tool_calls": node_type.max_tool_calls,
            }),
        )
        .await?;

        // ── System prompt = PURPOSE + FEAR + tool limit. Preloaded by code. ──
        let system = format!(
            "PURPOSE: {}\n\n\
            FEAR (violating these is failure):\n{}\n\n\
            You have {} tool calls. No more. Execute precisely.",
            purpose, fear, node_type.max_tool_calls,
        );

        // ── User message = EGO (context from parent) + task prompt ──
        let user_msg = if ego.is_empty() {
            prompt.clone()
        } else {
            format!("{}\n\n---\n\n{}", ego, prompt)
        };

        // Spawn agent: system preloaded, one user message, ≤N tool calls, done.
        let result = self.run_agent(run_id, addr, &system, &user_msg).await?;

        // Write output (code captures it)
        self.driver
            .write_artifact(run_id, addr, Artifact::Output, result.output.as_bytes())
            .await?;
        self.write_metrics(run_id, addr, &result).await?;

        // Evaluate success criterion
        let success = result.success && self.evaluate_success(node_type, &result.output);

        self.emit_event(
            run_id,
            addr,
            "leaf_complete",
            serde_json::json!({
                "node_type": node_type.id,
                "success": success,
                "tokens": result.tokens,
                "wall_ms": result.wall_ms,
            }),
        )
        .await?;

        manifest.finish_node(
            addr.as_str(),
            if success {
                NodeState::Success
            } else {
                NodeState::Failed
            },
            result.tokens,
        );
        self.write_manifest(run_id, manifest).await?;

        Ok(NodeResult {
            output: result.output,
            tokens: result.tokens,
            wall_ms: result.wall_ms,
            success,
            child_outputs: vec![],
        })
    }

    /// Execute a parent node: descend into children sequentially, aggregating context.
    async fn execute_parent(
        &self,
        run_id: &str,
        node_type: &NodeType,
        addr: &GraphAddress,
        system_prompt: &str,
        vars: &TemplateVars,
        manifest: &mut RunManifest,
    ) -> Result<NodeResult> {
        self.emit_event(
            run_id,
            addr,
            "parent_descending",
            serde_json::json!({
                "node_type": node_type.id,
                "children": node_type.children,
            }),
        )
        .await?;

        // ── Pre-hook: parent gathers context for children (CODE, not agent) ──
        let parent_context = self
            .gather_parent_context(run_id, node_type, addr, vars)
            .await?;

        let mut child_outputs: Vec<(String, String)> = Vec::new();
        // Seed with parent-gathered context so first child gets it
        if !parent_context.is_empty() {
            child_outputs.push(("_parent_context".into(), parent_context));
        }
        let mut total_tokens = 0usize;
        let mut total_wall_ms = 0u64;
        let mut all_success = true;

        for child_id in &node_type.children {
            let child_addr = addr.child(child_id);

            // Unblock child
            manifest.update_node(child_addr.as_str(), NodeState::Pending);
            self.write_manifest(run_id, manifest).await?;

            // Progressive context narrowing:
            // Each child gets the accumulated output of all previous siblings.
            let mut child_vars = vars.clone();
            child_vars.parent_output = child_outputs
                .iter()
                .map(|(id, out)| format!("### {} output:\n{}", id, out))
                .collect::<Vec<_>>()
                .join("\n\n");

            // If we have a plan from synthesize-plan, put it in {plan}
            if let Some((_, plan_text)) =
                child_outputs.iter().find(|(id, _)| id == "synthesize-plan")
            {
                child_vars.plan = plan_text.clone();
            }

            // If we have a branch name, extract it
            if let Some((_, branch_text)) =
                child_outputs.iter().find(|(id, _)| id == "create-branch")
            {
                // Try to extract branch name from output
                for line in branch_text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("fix-")
                        || trimmed.starts_with("feat-")
                        || trimmed.starts_with("issue-")
                    {
                        child_vars.branch = trimmed.to_string();
                        break;
                    }
                }
                if child_vars.branch.is_empty() {
                    child_vars.branch = format!("fix-{}-auto", vars.issue);
                }
            }
            if child_vars.branch.is_empty() {
                child_vars.branch = format!("fix-{}-auto", vars.issue);
            }

            let child_result = self
                .execute_node(
                    run_id,
                    child_id,
                    &child_addr,
                    system_prompt,
                    &child_vars,
                    manifest,
                )
                .await?;

            child_outputs.push((child_id.clone(), child_result.output.clone()));
            total_tokens += child_result.tokens;
            total_wall_ms += child_result.wall_ms;

            if !child_result.success {
                all_success = false;
                warn!(
                    run_id,
                    child = child_id,
                    "child failed — stopping parent descent"
                );
                // Mark remaining children as blocked
                let child_idx = node_type
                    .children
                    .iter()
                    .position(|c| c == child_id)
                    .unwrap_or(0);
                for remaining in &node_type.children[child_idx + 1..] {
                    let remaining_addr = addr.child(remaining);
                    manifest.update_node(remaining_addr.as_str(), NodeState::Blocked);
                }
                break;
            }
        }

        // Aggregate output
        let aggregated = child_outputs
            .iter()
            .map(|(id, out)| format!("## {}\n{}", id, out))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        self.driver
            .write_artifact(run_id, addr, Artifact::Output, aggregated.as_bytes())
            .await?;

        self.emit_event(
            run_id,
            addr,
            "parent_complete",
            serde_json::json!({
                "node_type": node_type.id,
                "success": all_success,
                "children_completed": child_outputs.len(),
                "children_total": node_type.children.len(),
                "tokens": total_tokens,
            }),
        )
        .await?;

        manifest.finish_node(
            addr.as_str(),
            if all_success {
                NodeState::Success
            } else {
                NodeState::Failed
            },
            total_tokens,
        );
        self.write_manifest(run_id, manifest).await?;

        Ok(NodeResult {
            output: aggregated,
            tokens: total_tokens,
            wall_ms: total_wall_ms,
            success: all_success,
            child_outputs,
        })
    }

    /// Parent pre-hook: CODE gathers context that children will receive as their universe.
    /// This is deterministic — no agent involved. Just code reading/fetching.
    async fn gather_parent_context(
        &self,
        run_id: &str,
        node_type: &NodeType,
        addr: &GraphAddress,
        vars: &TemplateVars,
    ) -> Result<String> {
        match node_type.id.as_str() {
            "analysis" => {
                // Analysis parent: fetch the issue text so children don't have to.
                // This is CODE calling gh, not an agent deciding to call gh.
                info!(run_id, "analysis pre-hook: fetching issue text");
                let issue = &vars.issue;
                let repo = &vars.repo;
                // Use the agent runtime to execute a single tool call deterministically
                // For now: shell out directly (code, not agent)
                let output = tokio::process::Command::new("gh")
                    .args([
                        "issue",
                        "view",
                        issue,
                        "--repo",
                        repo,
                        "--json",
                        "title,body,comments,labels,state",
                    ])
                    .output()
                    .await;
                match output {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                        self.driver
                            .write_artifact(run_id, addr, Artifact::Context, text.as_bytes())
                            .await?;
                        self.emit_event(
                            run_id,
                            addr,
                            "context_gathered",
                            serde_json::json!({
                                "source": format!("gh issue view {}", issue),
                                "bytes": text.len(),
                            }),
                        )
                        .await?;
                        Ok(text)
                    }
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr).to_string();
                        warn!(run_id, error = %err, "failed to fetch issue");
                        Ok(format!("ERROR: Could not fetch issue #{}: {}", issue, err))
                    }
                    Err(e) => {
                        warn!(run_id, error = %e, "gh command failed");
                        Ok(format!("ERROR: gh command failed: {}", e))
                    }
                }
            }
            "impl" => {
                // Impl parent: the plan is already in vars.plan from the analysis phase.
                // Just pass it through as context.
                Ok(vars.plan.clone())
            }
            "pr" => {
                // PR parent: the implementation output is in parent_output.
                Ok(vars.parent_output.clone())
            }
            _ => Ok(String::new()),
        }
    }

    /// Evaluate a leaf's success criterion against its output.
    fn evaluate_success(&self, node_type: &NodeType, output: &str) -> bool {
        // For now: simple substring checks based on success_criterion.
        // TODO: This should be a structured evaluation — possibly another agent.
        let Some(criterion) = &node_type.success_criterion else {
            return true; // No criterion = always pass
        };

        // Extract expected patterns from criterion
        // "Output contains ## PROBLEM, ## EXPECTED, and ## REPRODUCTION sections."
        // → check for ## PROBLEM, ## EXPECTED, ## REPRODUCTION
        if criterion.contains("contains") {
            // Extract quoted strings or section headers
            let checks: Vec<&str> = criterion
                .split(['\'', '"'])
                .enumerate()
                .filter(|(i, _)| i % 2 == 1) // odd indices are inside quotes
                .map(|(_, s)| s)
                .collect();

            if !checks.is_empty() {
                return checks.iter().all(|check| output.contains(check));
            }

            // Check for ## headers mentioned in criterion
            let headers: Vec<String> = criterion
                .split("##")
                .skip(1)
                .map(|s| {
                    format!(
                        "## {}",
                        s.split([',', '.', ' ']).next().unwrap_or("").trim()
                    )
                })
                .filter(|s| s.len() > 3)
                .collect();

            if !headers.is_empty() {
                return headers.iter().all(|h| output.contains(h.trim()));
            }
        }

        // Fallback: non-empty output = success
        !output.trim().is_empty()
    }

    /// Run a single agent and capture output.
    async fn run_agent(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<NodeResult> {
        let session_key = SessionKey::from(format!("kg:{}:{}", run_id, addr.as_str()));
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);

        let start = std::time::Instant::now();

        // Set system prompt on session
        {
            let session = self
                .runtime
                .sessions()
                .get_or_create(&session_key, Some(system_prompt));
            session.set_system_prompt(system_prompt).await;
        }

        let runtime = self.runtime.clone();
        let sk = session_key.clone();
        let prompt = user_prompt.to_string();

        let handle = tokio::spawn(async move { runtime.run_turn(&sk, &prompt, tx).await });

        // Collect output + emit events
        let mut output = String::new();
        let mut token_estimate = 0usize;
        let driver = self.driver.clone();
        let run_id_owned = run_id.to_string();
        let addr_clone = addr.clone();

        while let Some(event) = rx.recv().await {
            match &event {
                AgentEvent::Text(t) => {
                    output.push_str(t);
                    token_estimate += t.len() / 4;
                }
                AgentEvent::ToolExecuting { name, .. } => {
                    let _ = driver
                        .emit_event(
                            &run_id_owned,
                            KgEvent {
                                ts: Utc::now(),
                                graph_address: addr_clone.as_str().into(),
                                event: "tool_call".into(),
                                data: serde_json::json!({"tool": name}),
                            },
                        )
                        .await;
                }
                AgentEvent::ToolResult { name, is_error, .. } => {
                    let _ = driver
                        .emit_event(
                            &run_id_owned,
                            KgEvent {
                                ts: Utc::now(),
                                graph_address: addr_clone.as_str().into(),
                                event: "tool_result".into(),
                                data: serde_json::json!({"tool": name, "success": !is_error}),
                            },
                        )
                        .await;
                }
                AgentEvent::Error(e) => {
                    warn!(run_id = run_id_owned, error = %e, "agent error");
                }
                AgentEvent::Done { .. } => {}
                _ => {}
            }
        }

        let wall_ms = start.elapsed().as_millis() as u64;
        let success = handle.await?.is_ok();

        Ok(NodeResult {
            output,
            tokens: token_estimate,
            wall_ms,
            success,
            child_outputs: vec![],
        })
    }

    fn build_report(&self, run_id: &str, manifest: &RunManifest, _result: &NodeResult) -> String {
        let mut report = format!(
            "# KG Run Report: {}\n\n## Purpose\n{}\n\n## Target\n{}\n\n## Outcome: {}\n\n",
            run_id, manifest.purpose, manifest.target, manifest.outcome,
        );

        report.push_str("## Node Tree\n");
        for (addr, node) in &manifest.nodes {
            let indent = addr.matches('/').count().saturating_sub(2);
            let prefix = "  ".repeat(indent);
            let status_icon = match node.status {
                NodeState::Success => "✓",
                NodeState::Failed => "✗",
                NodeState::Running => "⟳",
                NodeState::Pending => "○",
                NodeState::Blocked => "▪",
                NodeState::Spawned => "◎",
            };
            report.push_str(&format!(
                "{}{} {} ({} tokens, {}ms)\n",
                prefix, status_icon, addr, node.tokens, node.wall_ms,
            ));
        }

        report.push_str(&format!(
            "\n## Totals\n- Tokens: {}\n- Wall: {}ms\n- Nodes: {}\n",
            manifest.total_tokens,
            manifest.total_wall_ms,
            manifest.nodes.len(),
        ));

        report
    }

    async fn write_manifest(&self, run_id: &str, manifest: &RunManifest) -> Result<()> {
        self.driver
            .write_artifact(
                run_id,
                &GraphAddress::root(),
                Artifact::Manifest,
                manifest.to_yaml().as_bytes(),
            )
            .await
    }

    async fn write_metrics(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        result: &NodeResult,
    ) -> Result<()> {
        self.driver
            .write_artifact(
                run_id,
                addr,
                Artifact::Metrics,
                serde_yaml::to_string(&serde_json::json!({
                    "tokens": result.tokens,
                    "wall_ms": result.wall_ms,
                    "outcome": if result.success { "success" } else { "failed" },
                }))?
                .as_bytes(),
            )
            .await
    }

    async fn write_report(&self, run_id: &str, manifest: &RunManifest, report: &str) -> Result<()> {
        self.driver
            .write_artifact(
                run_id,
                &GraphAddress::root(),
                Artifact::Report,
                report.as_bytes(),
            )
            .await?;
        let _ = self
            .driver
            .emit_event(
                run_id,
                KgEvent {
                    ts: Utc::now(),
                    graph_address: "/run-log".into(),
                    event: "run_complete".into(),
                    data: serde_json::json!({"line": manifest.run_log_line().trim()}),
                },
            )
            .await;
        Ok(())
    }

    async fn emit_event(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        event_name: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        self.driver
            .emit_event(
                run_id,
                KgEvent {
                    ts: Utc::now(),
                    graph_address: addr.as_str().into(),
                    event: event_name.into(),
                    data,
                },
            )
            .await
    }
}
