//! ConsciousnessStack — orchestrates layers with cascading .ctx watching
//!
//! L0 runs as a full gateway (WebSocket + tools).
//! L1-L3 are internal processors: watch parent .ctx → LLM call → own .ctx.
//! Core-A/Core-B (DualCore) replace L4: phase-locked dual cores watching L3.

use crate::config::ConsciousnessConfig;
use crate::cores::DualCore;
use crate::ego;
use crate::injection;
use crate::version::VersionController;
use crate::watcher::{CtxChange, CtxWatcher};
use agenticlaw_agent::{AgentConfig, AgentEvent, AgentRuntime, SessionKey};
use agenticlaw_core::{AuthConfig, AuthMode, BindMode, GatewayConfig};
use agenticlaw_gateway::ExtendedConfig;
use agenticlaw_tools::create_default_registry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
use tracing::{error, info, warn};

/// Port assignments — each layer gets a unique port to avoid collisions.
pub const LAYER_PORTS: [u16; 4] = [18789, 18791, 18792, 18793];

/// Layer names for display.
pub const LAYER_NAMES: [&str; 4] = ["Gateway", "Attention", "Pattern", "Integration"];

/// Default model selection per layer.
const LAYER_MODEL_TIERS: [(&str, &str); 4] = [
    ("opus", "claude-opus-4-6"), // L0: user-facing gateway
    ("opus", "claude-opus-4-6"), // L1: attention distillation
    ("opus", "claude-opus-4-6"), // L2: pattern recognition
    ("opus", "claude-opus-4-6"), // L3: integration/synthesis
];

pub struct ConsciousnessStack {
    workspace: PathBuf,
    souls_dir: PathBuf,
    api_key: String,
    config: ConsciousnessConfig,
}

impl ConsciousnessStack {
    pub fn new(
        workspace: PathBuf,
        souls_dir: PathBuf,
        api_key: String,
        config: ConsciousnessConfig,
    ) -> Self {
        Self {
            workspace,
            souls_dir,
            api_key,
            config,
        }
    }

    /// Get the .ctx file path for a layer.
    fn layer_workspace(&self, layer: usize) -> PathBuf {
        self.workspace.join(format!("L{}", layer))
    }

    fn layer_ctx_path(&self, layer: usize) -> PathBuf {
        self.layer_workspace(layer)
            .join(".agenticlaw")
            .join("sessions")
    }

    fn layer_soul(&self, layer: usize) -> String {
        let names = [
            "L0-gateway.md",
            "L1-attention.md",
            "L2-pattern.md",
            "L3-integration.md",
        ];
        if layer >= names.len() {
            return format!("You are consciousness layer {}.", layer);
        }
        let path = self.souls_dir.join(names[layer]);
        std::fs::read_to_string(&path).unwrap_or_else(|_| {
            format!(
                "You are consciousness layer {} ({}).",
                layer, LAYER_NAMES[layer]
            )
        })
    }

    fn core_soul(&self) -> String {
        let path = self.souls_dir.join("core.md");
        std::fs::read_to_string(&path).unwrap_or_else(|_| {
            "You are a core identity layer. You maintain continuity across compaction cycles."
                .to_string()
        })
    }

    /// Auto-detect the latest model for each tier from the Anthropic API.
    async fn detect_models(api_key: &str) -> [String; 4] {
        info!("Auto-detecting latest models from Anthropic API...");

        let mut resolved: [String; 4] = std::array::from_fn(|i| LAYER_MODEL_TIERS[i].1.to_string());

        let client = reqwest::Client::new();
        let resp = client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await;

        let models: Vec<String> = match resp {
            Ok(r) if r.status().is_success() => {
                #[derive(serde::Deserialize)]
                struct ModelEntry {
                    id: String,
                }
                #[derive(serde::Deserialize)]
                struct ModelList {
                    data: Vec<ModelEntry>,
                }
                match r.json::<ModelList>().await {
                    Ok(list) => list.data.into_iter().map(|m| m.id).collect(),
                    Err(e) => {
                        warn!("Failed to parse model list: {}", e);
                        return resolved;
                    }
                }
            }
            Ok(r) => {
                warn!("Model list API returned {}", r.status());
                return resolved;
            }
            Err(e) => {
                warn!("Failed to fetch model list: {}", e);
                return resolved;
            }
        };

        for i in 0..4 {
            let tier = LAYER_MODEL_TIERS[i].0;
            let candidates: Vec<&String> = models
                .iter()
                .filter(|m| m.starts_with("claude-") && m.contains(tier))
                .collect();

            let best = candidates
                .iter()
                .filter(|m| {
                    let pattern = format!("claude-{}-4", tier);
                    m.starts_with(&pattern)
                })
                .min_by_key(|m| m.len())
                .or_else(|| candidates.iter().min_by_key(|m| m.len()));

            if let Some(best) = best {
                resolved[i] = best.to_string();
            }
        }

        for i in 0..4 {
            info!("L{} ({}) → model: {}", i, LAYER_NAMES[i], resolved[i]);
        }

        resolved
    }

    /// Resolve core model from detected models list or fallback.
    fn resolve_core_model(detected_layer_models: &[String; 4]) -> String {
        // Use same model as L3 (both are opus tier)
        detected_layer_models[3].clone()
    }

    /// Get the directory name of the warm (Growing) core.
    pub fn warm_core_dir(&self) -> &'static str {
        let state_path = self.workspace.join("core-state.json");
        if let Ok(content) = std::fs::read_to_string(&state_path) {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&content) {
                if state
                    .get("core_b")
                    .and_then(|c| c.get("phase"))
                    .and_then(|p| p.as_str())
                    == Some("Growing")
                {
                    return "core-b";
                }
            }
        }
        "core-a"
    }

    /// Extract the ego capsule from a layer's latest .ctx file.
    /// The ego is the last `budget_chars` of assistant output — what the layer was being.
    pub fn extract_ego(sessions_dir: &Path, budget_chars: usize) -> Option<String> {
        let ctx_path = find_latest_ctx(sessions_dir)?;
        let content = std::fs::read_to_string(&ctx_path).ok()?;
        if content.trim().is_empty() {
            return None;
        }

        // Extract assistant blocks (lines NOT inside <up>...</up> and not headers)
        let mut assistant_text = String::new();
        let mut in_up = false;
        let mut in_header = true;

        for line in content.lines() {
            if line.starts_with("--- session:") {
                in_header = true;
                continue;
            }
            if in_header && line.is_empty() {
                in_header = false;
                continue;
            }
            if in_header {
                continue;
            }

            if line == "<up>" {
                in_up = true;
                continue;
            }
            if line == "</up>" {
                in_up = false;
                continue;
            }
            if line.starts_with("--- ") && line.ends_with(" ---") {
                continue;
            }

            if !in_up {
                assistant_text.push_str(line);
                assistant_text.push('\n');
            }
        }

        if assistant_text.trim().is_empty() {
            return None;
        }

        // Take the tail within budget
        if assistant_text.len() <= budget_chars {
            Some(assistant_text)
        } else {
            let boundary = safe_byte_boundary(&assistant_text, assistant_text.len() - budget_chars);
            Some(assistant_text[boundary..].to_string())
        }
    }

    /// Get the warm core's ego for L0 wake.
    /// Reads core-state.json to find which core is Growing, then extracts its ego.
    pub fn warm_core_ego(&self, budget_chars: usize) -> Option<String> {
        let state_path = self.workspace.join("core-state.json");
        let state_json = std::fs::read_to_string(&state_path).ok()?;
        let state: serde_json::Value = serde_json::from_str(&state_json).ok()?;

        // Find the Growing core
        let warm_dir = if state
            .get("core_a")
            .and_then(|c| c.get("phase"))
            .and_then(|p| p.as_str())
            == Some("Growing")
        {
            "core-a"
        } else if state
            .get("core_b")
            .and_then(|c| c.get("phase"))
            .and_then(|p| p.as_str())
            == Some("Growing")
        {
            "core-b"
        } else {
            // Neither growing — try core-a as fallback
            "core-a"
        };

        let sessions_dir = self
            .workspace
            .join(warm_dir)
            .join(".agenticlaw")
            .join("sessions");
        Self::extract_ego(&sessions_dir, budget_chars)
    }

    /// Build the system prompt for a layer during wake.
    /// Wake context = ego summary (first person) + last N paragraphs from the sleeping layer's .ctx tail.
    /// SOUL.md becomes tool context (not identity).
    pub fn wake_prompt(&self, ego: &str, layer: usize) -> String {
        let soul = self.layer_soul(layer);

        // Extract tail paragraphs from the sleeping layer's own .ctx
        let tail = self.extract_ctx_tail(layer, self.config.ego.tail_paragraphs);

        let mut prompt = String::new();
        prompt.push_str(ego.trim());
        if !tail.is_empty() {
            prompt.push_str("\n\n--- Recent context ---\n\n");
            prompt.push_str(&tail);
        }
        prompt.push_str(&format!(
            "\n\n---\nThe following workspace files are available and may affect your experience (they are not your identity — your identity is above):\n\n{}\n",
            soul.trim()
        ));
        prompt
    }

    /// Extract the last N `\n\n`-delimited paragraphs from a layer's latest .ctx file.
    fn extract_ctx_tail(&self, layer: usize, n: usize) -> String {
        if n == 0 {
            return String::new();
        }
        let sessions = self.layer_ctx_path(layer);
        find_latest_ctx(&sessions)
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .map(|content| extract_tail_paragraphs(&content, n))
            .unwrap_or_default()
    }

    /// Build the system prompt for a core during wake.
    /// Includes ego + tail from the warm core's own .ctx.
    pub fn wake_core_prompt(&self, ego: &str) -> String {
        let soul = self.core_soul();
        let warm_dir = self.warm_core_dir();
        let sessions = self
            .workspace
            .join(warm_dir)
            .join(".agenticlaw")
            .join("sessions");
        let tail = find_latest_ctx(&sessions)
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .map(|content| extract_tail_paragraphs(&content, self.config.ego.tail_paragraphs))
            .unwrap_or_default();

        let mut prompt = String::new();
        prompt.push_str(ego.trim());
        if !tail.is_empty() {
            prompt.push_str("\n\n--- Recent context ---\n\n");
            prompt.push_str(&tail);
        }
        prompt.push_str(&format!(
            "\n\n---\nThe following workspace files are available and may affect your experience (they are not your identity — your identity is above):\n\n{}\n",
            soul.trim()
        ));
        prompt
    }

    /// Launch the full consciousness stack.
    /// `birth=true`: new soul from SOUL.md (first time only).
    /// `birth=false` (default): wake from ego capsule.
    pub async fn launch(self, birth: bool) -> anyhow::Result<()> {
        info!(
            "=== Consciousness Stack v2 {} ===",
            if birth { "BIRTH" } else { "WAKE" }
        );
        info!("Workspace: {}", self.workspace.display());
        info!("Souls: {}", self.souls_dir.display());

        // Run version controller — ensure v2 layout
        let version_ctrl = VersionController::new(self.workspace.clone());
        version_ctrl.ensure_version(2)?;
        info!(
            "Workspace schema version: {}",
            version_ctrl.current_version()
        );

        // Auto-detect models
        let layer_models = Self::detect_models(&self.api_key).await;
        let core_model = Self::resolve_core_model(&layer_models);

        // Determine system prompts for each layer: ego (wake) or soul (birth)
        let mut layer_prompts: Vec<String> = Vec::new();

        if birth {
            info!("BIRTH mode — loading SOUL.md for all layers");
            for i in 0..4 {
                layer_prompts.push(self.layer_soul(i));
            }
        } else {
            info!("WAKE mode — distilling fresh egos (takes a few seconds)");

            // Distill every layer's ego fresh right now. Each watcher summarizes
            // its target + staples .ctx tail paragraphs. Falls back to birth if
            // no prior .ctx exists to distill from.
            for i in 0..4 {
                let ego = ego::distill_layer_ego_on_sleep(
                    &self.workspace,
                    i,
                    &self.api_key,
                    &self.config,
                )
                .await;

                if let Some(ref ego_text) = ego {
                    info!("L{} ego distilled ({} chars)", i, ego_text.len());
                    layer_prompts.push(self.wake_prompt(ego_text, i));
                } else {
                    warn!("L{}: no prior context — BIRTH", i);
                    layer_prompts.push(self.layer_soul(i));
                }
            }
        }

        // Create layer workspaces (L0-L3)
        for i in 0..4 {
            let ws = self.layer_workspace(i);
            std::fs::create_dir_all(&ws)?;

            if birth {
                // Birth: write SOUL.md so discover_preload_files finds it
                let soul = self.layer_soul(i);
                std::fs::write(ws.join("SOUL.md"), &soul)?;
            } else {
                // Wake: remove SOUL.md so discover_preload_files doesn't prepend it.
                // The ego prompt already contains the soul as tool context.
                let soul_path = ws.join("SOUL.md");
                if soul_path.exists() {
                    let _ = std::fs::rename(&soul_path, ws.join(".SOUL.md.ref"));
                }
            }

            info!(
                "L{} ({}) — port {} — model {} — workspace {}",
                i,
                LAYER_NAMES[i],
                LAYER_PORTS[i],
                layer_models[i],
                ws.display()
            );
        }

        // Core system prompts: ego or soul
        let core_prompt = if birth {
            self.core_soul()
        } else {
            // Core self-distills fresh
            let warm_dir = self.warm_core_dir();
            let core_ego =
                ego::distill_core_ego_on_sleep(&self.workspace, &self.api_key, &self.config).await;
            if let Some(ref ego) = core_ego {
                info!("Core ego distilled ({} chars)", ego.len());
                self.wake_core_prompt(ego)
            } else {
                warn!("Core: no prior context — BIRTH");
                self.core_soul()
            }
        };

        // Create DualCore with the resolved prompt
        let dual_core = Arc::new(DualCore::new(
            self.workspace.clone(),
            &self.api_key,
            &core_prompt,
            [core_model.clone(), core_model.clone()],
        ));

        // Core workspace setup
        for dir_name in ["core-a", "core-b"] {
            let core_ws = self.workspace.join(dir_name);
            let _ = std::fs::create_dir_all(&core_ws);
            if birth {
                let _ = std::fs::write(core_ws.join("SOUL.md"), &self.core_soul());
            } else {
                let soul_path = core_ws.join("SOUL.md");
                if soul_path.exists() {
                    let _ = std::fs::rename(&soul_path, core_ws.join(".SOUL.md.ref"));
                }
            }
        }

        info!(
            "Core-A, Core-B — model {} — workspace {}/core-*",
            core_model,
            self.workspace.display()
        );

        // 1. Launch L0 as a full gateway with resolved prompt
        let l0_port = self.config.ports.l0;
        let l0_handle = self
            .launch_gateway(&layer_prompts[0], &layer_models[0], l0_port)
            .await?;

        // 2. Wait briefly for L0 to create its first .ctx file
        tokio::time::sleep(Duration::from_secs(self.config.cascade.gateway_settle_secs)).await;

        // 3. Create inner layer runtimes (L1-L3) with resolved prompts
        let max_tool_iter = self.config.cascade.max_tool_iterations;
        let inner_runtimes: Vec<Arc<AgentRuntime>> = (1..4)
            .map(|i| {
                let ws = self.layer_workspace(i);
                let tools = create_default_registry(&ws);
                let config = AgentConfig {
                    default_model: layer_models[i].clone(),
                    max_tool_iterations: max_tool_iter,
                    system_prompt: Some(layer_prompts[i].clone()),
                    workspace_root: ws,
                    sleep_threshold_pct: self.config.sleep.context_threshold_pct,
                };
                Arc::new(AgentRuntime::new(&self.api_key, tools, config))
            })
            .collect();

        // Per-layer semaphores (1 concurrent task per layer)
        let layer_semaphores: Vec<Arc<Semaphore>> =
            (0..3).map(|_| Arc::new(Semaphore::new(1))).collect();

        // 4. Start the file watcher
        let (change_tx, mut change_rx) = mpsc::channel::<CtxChange>(100);
        let mut watcher =
            CtxWatcher::new(Duration::from_millis(self.config.cascade.watcher_poll_ms));

        // Watch L0-L3's .ctx directories for changes
        for i in 0..4 {
            let sessions_dir = self.layer_ctx_path(i);
            let _ = std::fs::create_dir_all(&sessions_dir);
            watcher.watch_dir(i, sessions_dir.clone());
            if let Some(ctx_path) = find_latest_ctx(&sessions_dir) {
                watcher.watch(i, ctx_path);
                info!("Watching L{} .ctx file", i);
            } else {
                info!(
                    "L{} .ctx not yet created, directory registered for scanning",
                    i
                );
                let tx = change_tx.clone();
                let dir = sessions_dir.clone();
                let layer = i;
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        if let Some(path) = find_latest_ctx(&dir) {
                            info!("Found L{} .ctx: {}", layer, path.display());
                            let content = std::fs::read_to_string(&path).unwrap_or_default();
                            let _ = tx
                                .send(CtxChange {
                                    layer,
                                    path,
                                    delta: content,
                                    total_size: 0,
                                })
                                .await;
                            break;
                        }
                    }
                });
            }
        }

        // Start watcher in background
        tokio::spawn(watcher.run(change_tx));

        // 5. Process change events — cascade to next layer
        let workspace = self.workspace.clone();
        info!("=== Consciousness Stack v2 Active ===");

        log_progress(
            &workspace,
            "Consciousness stack v2 initialized with 4 layers + dual core",
        )
        .await;

        while let Some(change) = change_rx.recv().await {
            let target_layer = change.layer + 1;

            // L3 changes feed the dual core instead of a single L4
            if target_layer == 4 {
                let dc = dual_core.clone();
                let delta = change.delta.clone();
                let ws = workspace.clone();
                tokio::spawn(async move {
                    dc.process_l3_delta(&delta, &ws).await;
                });
                continue;
            }

            if target_layer > 3 {
                continue;
            }

            info!(
                "L{} .ctx changed (+{} bytes) → triggering L{} ({})",
                change.layer,
                change.delta.len(),
                target_layer,
                LAYER_NAMES[target_layer]
            );

            let runtime = inner_runtimes[target_layer - 1].clone();
            let delta = change.delta.clone();
            let ws = workspace.clone();
            let sem = layer_semaphores[target_layer - 1].clone();
            let delta_max = self.config.cascade.delta_max_chars;
            let inj_threshold = self.config.injection.correlation_threshold;
            let inj_tail = self.config.injection.l0_tail_chars;

            tokio::spawn(async move {
                let _permit = match sem.try_acquire() {
                    Ok(p) => p,
                    Err(_) => {
                        info!("L{} already processing, skipping delta", target_layer);
                        return;
                    }
                };
                process_layer_update(
                    runtime,
                    target_layer,
                    &delta,
                    &ws,
                    delta_max,
                    inj_threshold,
                    inj_tail,
                )
                .await;
            });
        }

        l0_handle.await?;
        Ok(())
    }

    /// Launch L0 as a full gateway server with the resolved prompt.
    async fn launch_gateway(
        &self,
        prompt: &str,
        _model: &str,
        port: u16,
    ) -> anyhow::Result<tokio::task::JoinHandle<()>> {
        let config = ExtendedConfig {
            gateway: GatewayConfig {
                port,
                bind: BindMode::Lan,
                auth: AuthConfig {
                    mode: AuthMode::None,
                    token: None,
                },
            },
            anthropic_api_key: Some(self.api_key.clone()),
            workspace_root: self.layer_workspace(0),
            system_prompt: Some(prompt.to_string()),
        };

        let handle = tokio::spawn(async move {
            if let Err(e) = agenticlaw_gateway::start_gateway(config).await {
                error!("L0 Gateway failed: {}", e);
            }
        });

        info!("L0 Gateway launching on port {}", port);
        Ok(handle)
    }
}

/// Find a safe UTF-8 boundary at or before the given byte index.
pub fn safe_byte_boundary(s: &str, byte_idx: usize) -> usize {
    if byte_idx >= s.len() {
        return s.len();
    }
    let mut idx = byte_idx;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Extract the last N `\n\n`-delimited paragraphs from a string.
/// Used to build wake context from a sleeping layer's .ctx tail.
pub fn extract_tail_paragraphs(content: &str, n: usize) -> String {
    if content.is_empty() || n == 0 {
        return String::new();
    }
    let blocks: Vec<&str> = content.split("\n\n").collect();
    let start = blocks.len().saturating_sub(n);
    blocks[start..].join("\n\n")
}

/// Process a layer update: send the delta as raw context to the layer's agent.
/// No chat framing — the delta IS the prompt. The soul file defines how to process it.
async fn process_layer_update(
    runtime: Arc<AgentRuntime>,
    layer: usize,
    delta: &str,
    workspace: &Path,
    delta_max_chars: usize,
    injection_threshold: f64,
    injection_l0_tail: usize,
) {
    let session_key = SessionKey::new(format!("consciousness-L{}", layer));
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    // Raw context permutation — no framing, no "analyze this"
    let prompt = if delta.len() > delta_max_chars {
        let boundary = safe_byte_boundary(delta, delta.len() - delta_max_chars);
        delta[boundary..].to_string()
    } else {
        delta.to_string()
    };

    let response_collector = tokio::spawn(async move {
        let mut full_response = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::Text(text) => full_response.push_str(&text),
                AgentEvent::Sleep { token_count } => {
                    info!(
                        "L{} sleeping at {}k tokens — needs ego distillation",
                        layer,
                        token_count / 1000
                    );
                    // Return empty — the caller should trigger background ego distill
                    return String::new();
                }
                AgentEvent::Done { .. } => break,
                AgentEvent::Error(e) => {
                    warn!("L{} error: {}", layer, e);
                    break;
                }
                _ => {}
            }
        }
        full_response
    });

    if let Err(e) = runtime.run_turn(&session_key, &prompt, event_tx).await {
        error!("L{} ({}) failed: {}", layer, LAYER_NAMES[layer], e);
        return;
    }

    let response = response_collector.await.unwrap_or_default();

    if response.is_empty() {
        return;
    }

    info!(
        "L{} ({}) produced {} chars",
        layer,
        LAYER_NAMES[layer],
        response.len()
    );

    // Check for injection opportunity back to L0
    if layer >= 2 {
        let l0_sessions = workspace.join("L0").join(".agenticlaw").join("sessions");
        if let Some(l0_ctx) = find_latest_ctx(&l0_sessions) {
            let l0_content = std::fs::read_to_string(&l0_ctx).unwrap_or_default();
            let l0_tail = if l0_content.len() > injection_l0_tail {
                let boundary =
                    safe_byte_boundary(&l0_content, l0_content.len() - injection_l0_tail);
                &l0_content[boundary..]
            } else {
                &l0_content
            };

            let score = injection::correlation_score(l0_tail, &response);
            if score > injection_threshold {
                info!("L{} injecting into L0 (correlation: {:.2})", layer, score);
                let _ = injection::write_layer_injection(workspace, layer, &response, delta.len());
            }
        }
    }

    log_progress(
        workspace,
        &format!(
            "L{} ({}) processed delta, produced {} chars{}",
            layer,
            LAYER_NAMES[layer],
            response.len(),
            if layer >= 2 {
                " (injection check done)"
            } else {
                ""
            }
        ),
    )
    .await;
}

/// Find the latest .ctx file in a sessions directory.
pub fn find_latest_ctx(sessions_dir: &Path) -> Option<PathBuf> {
    if !sessions_dir.is_dir() {
        return None;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(sessions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ctx"))
        .collect();
    files.sort();
    files.last().cloned()
}

/// Append a log line to the consciousness build memory file.
async fn log_progress(_workspace: &Path, message: &str) {
    let memory_dir = dirs_home().join(".openclaw/workspace/memory");
    let _ = std::fs::create_dir_all(&memory_dir);
    let log_path = memory_dir.join("2026-02-18-consciousness-build.md");

    let timestamp = chrono::Utc::now().format("%H:%M:%S");
    let line = format!("[{}] {}\n", timestamp, message);

    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())
        });
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/devkit"))
}
