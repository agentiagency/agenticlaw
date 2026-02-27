//! DualCore — phase-locked dual core system replacing single L4
//!
//! Two cores (A and B) alternate between growing and compacting.
//! At any time, one core has deep context while the other recovers.

use crate::injection;
use agenticlaw_agent::{AgentConfig, AgentEvent, AgentRuntime, SessionKey};
use agenticlaw_tools::create_default_registry;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tracing::{error, info, warn};

/// Ports for Core-A and Core-B
pub const CORE_PORTS: [u16; 2] = [18794, 18795];
pub const CORE_NAMES: [&str; 2] = ["Core-A", "Core-B"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreId {
    A,
    B,
}

impl CoreId {
    pub fn other(self) -> Self {
        match self {
            CoreId::A => CoreId::B,
            CoreId::B => CoreId::A,
        }
    }

    pub fn index(self) -> usize {
        match self {
            CoreId::A => 0,
            CoreId::B => 1,
        }
    }

    pub fn dir_name(self) -> &'static str {
        match self {
            CoreId::A => "core-a",
            CoreId::B => "core-b",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorePhase {
    Growing,
    Ready,
    Compacting,
    Infant,
    Seeded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleCoreState {
    pub phase: CorePhase,
    pub estimated_tokens: usize,
    pub samples: usize,
    pub skip_counter: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreState {
    pub version: u32,
    pub core_a: SingleCoreState,
    pub core_b: SingleCoreState,
    pub budget_tokens: usize,
    pub last_compaction_core: Option<CoreId>,
    pub last_compaction_time: Option<String>,
}

impl CoreState {
    pub fn new(budget_tokens: usize) -> Self {
        Self {
            version: 2,
            core_a: SingleCoreState {
                phase: CorePhase::Growing,
                estimated_tokens: 0,
                samples: 0,
                skip_counter: 0,
            },
            core_b: SingleCoreState {
                phase: CorePhase::Infant,
                estimated_tokens: 0,
                samples: 0,
                skip_counter: 0,
            },
            budget_tokens,
            last_compaction_core: None,
            last_compaction_time: None,
        }
    }

    pub fn core(&self, id: CoreId) -> &SingleCoreState {
        match id {
            CoreId::A => &self.core_a,
            CoreId::B => &self.core_b,
        }
    }

    pub fn core_mut(&mut self, id: CoreId) -> &mut SingleCoreState {
        match id {
            CoreId::A => &mut self.core_a,
            CoreId::B => &mut self.core_b,
        }
    }
}

/// Estimate tokens from text (chars / 4)
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Find a safe UTF-8 boundary at or before the given byte index.
fn safe_byte_boundary(s: &str, byte_idx: usize) -> usize {
    if byte_idx >= s.len() {
        return s.len();
    }
    let mut idx = byte_idx;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub struct DualCore {
    runtimes: [Arc<AgentRuntime>; 2],
    state: Arc<Mutex<CoreState>>,
    workspace: PathBuf,
    state_path: PathBuf,
    semaphores: [Arc<Semaphore>; 2],
    ready_since: Arc<Mutex<[Option<Instant>; 2]>>,
}

impl DualCore {
    pub fn new(workspace: PathBuf, api_key: &str, soul: &str, models: [String; 2]) -> Self {
        let runtimes = std::array::from_fn(|i| {
            let core_ws = workspace.join(CoreId::from_index(i).dir_name());
            let _ = std::fs::create_dir_all(&core_ws);
            let tools = create_default_registry(&core_ws);
            let config = AgentConfig {
                default_model: models[i].clone(),
                max_tool_iterations: 3,
                system_prompt: Some(soul.to_string()),
                workspace_root: core_ws,
                sleep_threshold_pct: 1.0,
            };
            Arc::new(AgentRuntime::new(api_key, tools, config))
        });

        let state_path = workspace.join("core-state.json");
        let state = Self::hydrate_or_create(&state_path, 200_000);

        Self {
            runtimes,
            state: Arc::new(Mutex::new(state)),
            workspace,
            state_path,
            semaphores: [Arc::new(Semaphore::new(1)), Arc::new(Semaphore::new(1))],
            ready_since: Arc::new(Mutex::new([None, None])),
        }
    }

    fn hydrate_or_create(state_path: &Path, budget: usize) -> CoreState {
        if state_path.exists() {
            match std::fs::read_to_string(state_path) {
                Ok(json) => match serde_json::from_str::<CoreState>(&json) {
                    Ok(state) => {
                        info!(
                            "Hydrated core state from checkpoint: A={:?} B={:?}",
                            state.core_a.phase, state.core_b.phase
                        );
                        return state;
                    }
                    Err(e) => warn!("Failed to parse core-state.json: {}, creating fresh", e),
                },
                Err(e) => warn!("Failed to read core-state.json: {}, creating fresh", e),
            }
        }
        CoreState::new(budget)
    }

    /// Process an L3 delta — route to appropriate core(s) based on phase.
    pub async fn process_l3_delta(&self, delta: &str, workspace: &Path) {
        // Check for Ready timeout (30s) first
        self.check_ready_timeout().await;

        // Determine which cores should receive this delta
        for core_id in [CoreId::A, CoreId::B] {
            let should_process = {
                let state = self.state.lock().await;
                let core = state.core(core_id);
                match core.phase {
                    CorePhase::Growing => true,
                    CorePhase::Seeded => true,
                    _ => false,
                }
            };

            if !should_process {
                continue;
            }

            // Phase-locked growth: small core samples at ½ rate
            let should_sample = {
                let mut state = self.state.lock().await;
                let other_tokens = state.core(core_id.other()).estimated_tokens;
                let my_tokens = state.core(core_id).estimated_tokens;
                let i_am_small = my_tokens < other_tokens;

                if i_am_small {
                    let core = state.core_mut(core_id);
                    core.skip_counter += 1;
                    if core.skip_counter % 2 == 0 {
                        true // sample every other delta
                    } else {
                        false
                    }
                } else {
                    true // big core always samples
                }
            };

            if !should_sample {
                continue;
            }

            let sem = self.semaphores[core_id.index()].clone();
            let permit = match sem.try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    info!(
                        "{} already processing, skipping delta",
                        CORE_NAMES[core_id.index()]
                    );
                    continue;
                }
            };

            let runtime = self.runtimes[core_id.index()].clone();
            let delta_owned = delta.to_string();
            let state_arc = self.state.clone();
            let ws = workspace.to_path_buf();
            let self_ws = self.workspace.clone();
            let state_path = self.state_path.clone();
            let ready_since = self.ready_since.clone();

            tokio::spawn(async move {
                let _permit = permit;

                // Bound delta with safe UTF-8 boundary
                let bounded = if delta_owned.len() > 4000 {
                    let boundary = safe_byte_boundary(&delta_owned, delta_owned.len() - 4000);
                    &delta_owned[boundary..]
                } else {
                    &delta_owned
                };

                let response = run_core_turn(&runtime, core_id, bounded).await;
                if response.is_empty() {
                    return;
                }

                info!(
                    "{} produced {} chars",
                    CORE_NAMES[core_id.index()],
                    response.len()
                );

                let delta_tokens = estimate_tokens(&response);

                // Update state
                let mut state = state_arc.lock().await;

                // Transition Seeded → Growing on first sample
                if state.core(core_id).phase == CorePhase::Seeded {
                    state.core_mut(core_id).phase = CorePhase::Growing;
                }

                let core = state.core_mut(core_id);
                core.estimated_tokens += delta_tokens;
                core.samples += 1;

                let budget_half = state.budget_tokens / 2;
                let my_tokens = state.core(core_id).estimated_tokens;

                // Check if we've hit the threshold
                if my_tokens >= budget_half && state.core(core_id).phase == CorePhase::Growing {
                    state.core_mut(core_id).phase = CorePhase::Ready;
                    ready_since.lock().await[core_id.index()] = Some(Instant::now());
                    info!(
                        "{} reached budget half ({} tokens), entering Ready phase",
                        CORE_NAMES[core_id.index()],
                        my_tokens
                    );
                }

                // Attempt compaction handshake if we're Ready
                if state.core(core_id).phase == CorePhase::Ready {
                    let peer = core_id.other();
                    let peer_phase = state.core(peer).phase;

                    if peer_phase == CorePhase::Growing {
                        // Peer can approve — begin compaction
                        info!(
                            "{} approved compaction of {}",
                            CORE_NAMES[peer.index()],
                            CORE_NAMES[core_id.index()]
                        );
                        state.core_mut(core_id).phase = CorePhase::Compacting;
                        checkpoint_state(&state_path, &state);

                        // Select seed from compacting core for peer
                        let seed = select_seed_from_response(&response, budget_half / 10);
                        info!(
                            "{} compacting, seed {} chars for {}",
                            CORE_NAMES[core_id.index()],
                            seed.len(),
                            CORE_NAMES[peer.index()]
                        );

                        // Write seed as injection into the peer's workspace
                        let peer_dir = self_ws.join(peer.dir_name());
                        let _ = std::fs::create_dir_all(&peer_dir);
                        let seed_file = peer_dir.join("seed.txt");
                        let _ = std::fs::write(&seed_file, &seed);

                        // Transition: compacting core → Infant, peer absorbs seed
                        state.core_mut(core_id).phase = CorePhase::Infant;
                        state.core_mut(core_id).estimated_tokens = 0;
                        state.core_mut(core_id).samples = 0;
                        state.core_mut(core_id).skip_counter = 0;
                        state.last_compaction_core = Some(core_id);
                        state.last_compaction_time = Some(chrono::Utc::now().to_rfc3339());
                    }
                }

                // Check tie-breaker: if both Ready, fewer samples compacts first
                if state.core_a.phase == CorePhase::Ready && state.core_b.phase == CorePhase::Ready
                {
                    let compactor = if state.core_a.samples <= state.core_b.samples {
                        CoreId::A
                    } else {
                        CoreId::B
                    };
                    let approver = compactor.other();
                    info!(
                        "Tie-breaker: {} compacts (fewer samples: {} vs {})",
                        CORE_NAMES[compactor.index()],
                        state.core(compactor).samples,
                        state.core(approver).samples
                    );

                    state.core_mut(compactor).phase = CorePhase::Compacting;
                    state.core_mut(approver).phase = CorePhase::Growing; // approver resumes

                    // Compact
                    let seed = select_seed_from_response(&response, budget_half / 10);
                    let peer_dir = self_ws.join(approver.dir_name());
                    let _ = std::fs::create_dir_all(&peer_dir);
                    let _ = std::fs::write(peer_dir.join("seed.txt"), &seed);

                    state.core_mut(compactor).phase = CorePhase::Infant;
                    state.core_mut(compactor).estimated_tokens = 0;
                    state.core_mut(compactor).samples = 0;
                    state.core_mut(compactor).skip_counter = 0;
                    state.last_compaction_core = Some(compactor);
                    state.last_compaction_time = Some(chrono::Utc::now().to_rfc3339());
                }

                // Checkpoint
                checkpoint_state(&state_path, &state);

                // Check injection into L0
                let l0_sessions = ws.join("L0").join(".agenticlaw").join("sessions");
                if let Some(l0_ctx) = super::stack::find_latest_ctx(&l0_sessions) {
                    let l0_content = std::fs::read_to_string(&l0_ctx).unwrap_or_default();
                    let l0_tail = if l0_content.len() > 2000 {
                        let boundary = safe_byte_boundary(&l0_content, l0_content.len() - 2000);
                        &l0_content[boundary..]
                    } else {
                        &l0_content
                    };

                    let score = injection::correlation_score(l0_tail, &response);
                    if score > 0.1 {
                        info!(
                            "{} injecting into L0 (correlation: {:.2})",
                            CORE_NAMES[core_id.index()],
                            score
                        );
                        let _ =
                            injection::write_injection(&ws, core_id, &response, delta_owned.len());
                    }
                }

                // Check if Infant core has a seed file to absorb
                for cid in [CoreId::A, CoreId::B] {
                    if state.core(cid).phase == CorePhase::Infant {
                        let seed_file = self_ws.join(cid.dir_name()).join("seed.txt");
                        if seed_file.exists() {
                            if let Ok(seed) = std::fs::read_to_string(&seed_file) {
                                info!(
                                    "{} absorbing seed ({} chars)",
                                    CORE_NAMES[cid.index()],
                                    seed.len()
                                );
                                state.core_mut(cid).phase = CorePhase::Seeded;
                                state.core_mut(cid).estimated_tokens = estimate_tokens(&seed);
                                let _ = std::fs::remove_file(&seed_file);
                                checkpoint_state(&state_path, &state);
                            }
                        }
                    }
                }
            });
        }
    }

    async fn check_ready_timeout(&self) {
        let mut ready = self.ready_since.lock().await;
        let mut state = self.state.lock().await;

        for core_id in [CoreId::A, CoreId::B] {
            if let Some(since) = ready[core_id.index()] {
                if since.elapsed() > Duration::from_secs(30) {
                    if state.core(core_id).phase == CorePhase::Ready {
                        warn!(
                            "{} Ready timeout (30s), reverting to Growing",
                            CORE_NAMES[core_id.index()]
                        );
                        state.core_mut(core_id).phase = CorePhase::Growing;
                        ready[core_id.index()] = None;
                        checkpoint_state(&self.state_path, &state);
                    }
                }
            }
        }
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

impl CoreId {
    fn from_index(i: usize) -> Self {
        match i {
            0 => CoreId::A,
            _ => CoreId::B,
        }
    }
}

fn checkpoint_state(path: &Path, state: &CoreState) {
    let json = match serde_json::to_string_pretty(state) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to serialize core state: {}", e);
            return;
        }
    };
    let tmp_path = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp_path, &json) {
        error!("Failed to write core state tmp: {}", e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        error!("Failed to rename core state checkpoint: {}", e);
    }
}

async fn run_core_turn(runtime: &AgentRuntime, core_id: CoreId, prompt: &str) -> String {
    let session_key = SessionKey::new(format!("consciousness-{}", CORE_NAMES[core_id.index()]));
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    let core_name = CORE_NAMES[core_id.index()];
    let collector = tokio::spawn(async move {
        let mut full_response = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::Text(text) => full_response.push_str(&text),
                AgentEvent::Done { .. } => break,
                AgentEvent::Error(e) => {
                    warn!("{} error: {}", core_name, e);
                    break;
                }
                _ => {}
            }
        }
        full_response
    });

    if let Err(e) = runtime.run_turn(&session_key, prompt, event_tx).await {
        error!("{} failed: {}", CORE_NAMES[core_id.index()], e);
        return String::new();
    }

    collector.await.unwrap_or_default()
}

/// Entropy-law seed selection: pick the most information-dense paragraphs
/// that fit within the token budget.
fn select_seed_from_response(text: &str, budget_tokens: usize) -> String {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if paragraphs.is_empty() {
        return text.to_string();
    }

    // Score each paragraph by information density with recency bias
    let total_paragraphs = paragraphs.len();
    let mut scored: Vec<(usize, f64, &str)> = paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let words: Vec<&str> = p.split_whitespace().collect();
            let total_terms = words.len().max(1);
            let unique_terms: HashSet<&str> = words
                .iter()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                .filter(|w| !w.is_empty())
                .collect();

            let density = unique_terms.len() as f64 / total_terms as f64;

            // Recency bias: paragraphs later in the text score higher
            let recency = (i as f64 + 1.0) / total_paragraphs as f64;

            let score = density * 0.7 + recency * 0.3;
            (i, score, *p)
        })
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Select top-K paragraphs within budget (within 10% of budget)
    let budget_chars = budget_tokens * 4; // reverse estimate
    let max_chars = budget_chars + budget_chars / 10;
    let mut selected: Vec<(usize, &str)> = Vec::new();
    let mut total_chars = 0;

    for (idx, _score, para) in &scored {
        if total_chars + para.len() > max_chars {
            continue;
        }
        selected.push((*idx, para));
        total_chars += para.len();
    }

    // Sort by original order to maintain coherence
    selected.sort_by_key(|(idx, _)| *idx);

    selected
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>()
        .join("\n\n")
}
#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // estimate_tokens — chars/4 approximation
    // ============================================================

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_basic() {
        // 100 chars / 4 = 25 tokens
        let text = "a".repeat(100);
        assert_eq!(estimate_tokens(&text), 25);
    }

    #[test]
    fn estimate_tokens_rounds_down() {
        // 7 chars / 4 = 1 (integer division)
        assert_eq!(estimate_tokens("abcdefg"), 1);
    }

    // ============================================================
    // safe_byte_boundary — UTF-8 safety
    // ============================================================

    #[test]
    fn safe_boundary_ascii() {
        let s = "hello world";
        assert_eq!(safe_byte_boundary(s, 5), 5);
    }

    #[test]
    fn safe_boundary_at_string_end() {
        let s = "hello";
        assert_eq!(safe_byte_boundary(s, 100), 5);
    }

    #[test]
    fn safe_boundary_multibyte() {
        let s = "hi\u{1F980}bye"; // 🦀 is 4 bytes
                                  // "hi" = 2 bytes, crab = 4 bytes (bytes 2..6), "bye" = 3 bytes
                                  // Asking for boundary at byte 4 (middle of crab) should back up to 2
        assert_eq!(safe_byte_boundary(s, 4), 2);
        assert_eq!(safe_byte_boundary(s, 3), 2);
        // Byte 2 is the start of crab, that is a valid boundary
        assert_eq!(safe_byte_boundary(s, 2), 2);
        // Byte 6 is start of "bye"
        assert_eq!(safe_byte_boundary(s, 6), 6);
    }

    #[test]
    fn safe_boundary_zero() {
        let s = "\u{1F980}";
        assert_eq!(safe_byte_boundary(s, 0), 0);
    }

    // ============================================================
    // select_seed_from_response — entropy-law seed selection
    // ============================================================

    #[test]
    fn seed_selection_empty_returns_input() {
        let result = select_seed_from_response("", 100);
        assert_eq!(result, "");
    }

    #[test]
    fn seed_selection_single_paragraph() {
        let text = "This is a single paragraph with enough words to test.";
        let result = select_seed_from_response(text, 1000);
        assert_eq!(result, text);
    }

    #[test]
    fn seed_selection_respects_budget() {
        // Create text with multiple paragraphs that exceed budget
        let p1 = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let p2 = "kilo lima mike november oscar papa quebec romeo sierra tango";
        let p3 = "uniform victor whiskey xray yankee zulu ability bracket courage drum";
        let text = format!("{}\n\n{}\n\n{}", p1, p2, p3);

        // Budget of 5 tokens = 20 chars + 10% = 22 chars max. No full paragraph fits.
        let result = select_seed_from_response(&text, 5);
        assert!(
            result.len() <= 22,
            "Seed should respect budget, got {} chars",
            result.len()
        );
    }

    #[test]
    fn seed_selection_preserves_original_order() {
        // Even though paragraphs are scored and sorted by score,
        // the final output should restore original document order.
        let text = "first paragraph here\n\nsecond paragraph here\n\nthird paragraph here";
        let result = select_seed_from_response(text, 1000);

        if result.contains("first") && result.contains("third") {
            let first_pos = result.find("first").unwrap();
            let third_pos = result.find("third").unwrap();
            assert!(
                first_pos < third_pos,
                "Original order should be preserved in seed"
            );
        }
    }

    #[test]
    fn seed_selection_favors_density_and_recency() {
        // Paragraph with all unique words (high density) vs repetitive paragraph
        let unique_para =
            "consciousness gateway injection correlation threshold cascade delta watcher";
        let repetitive_para = "word word word word word word word word word word";
        // Put unique paragraph LAST for recency bonus too
        let text = format!("{}\n\n{}", repetitive_para, unique_para);

        let result = select_seed_from_response(&text, 100);
        // Should strongly prefer the unique paragraph
        assert!(
            result.contains("consciousness"),
            "Seed should favor high-density paragraph, got: {}",
            result
        );
    }

    #[test]
    fn seed_selection_no_empty_paragraphs() {
        let text = "real content here\n\n\n\n\n\nmore content here";
        let result = select_seed_from_response(text, 1000);
        assert!(!result.is_empty());
        assert!(result.contains("real content"));
    }

    // ============================================================
    // CorePhase — phase transition validity
    // ============================================================

    #[test]
    fn phase_lifecycle_normal_sequence() {
        // Valid lifecycle: Infant -> Seeded -> Growing -> Ready -> Compacting -> Infant
        let phases = [
            CorePhase::Infant,
            CorePhase::Seeded,
            CorePhase::Growing,
            CorePhase::Ready,
            CorePhase::Compacting,
            CorePhase::Infant, // cycle complete
        ];

        // All phases should be distinct (except the bookend Infant)
        for i in 0..phases.len() - 1 {
            for j in (i + 1)..phases.len() - 1 {
                assert_ne!(phases[i], phases[j], "Phases {} and {} should differ", i, j);
            }
        }
        // Bookends match — the cycle closes
        assert_eq!(phases[0], phases[phases.len() - 1]);
    }

    // ============================================================
    // CoreState — budget threshold calculation
    // ============================================================

    #[test]
    fn budget_half_calculation() {
        let state = CoreState::new(200_000);
        let budget_half = state.budget_tokens / 2;
        assert_eq!(budget_half, 100_000);
    }

    #[test]
    fn seed_budget_is_tenth_of_half() {
        let state = CoreState::new(200_000);
        let budget_half = state.budget_tokens / 2;
        let seed_budget = budget_half / 10;
        assert_eq!(seed_budget, 10_000);
    }

    // ============================================================
    // CoreState — checkpoint file format
    // ============================================================

    #[test]
    fn checkpoint_produces_valid_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("core-state.json");

        let mut state = CoreState::new(200_000);
        state.core_mut(CoreId::A).estimated_tokens = 50_000;
        state.core_mut(CoreId::A).samples = 100;
        state.core_mut(CoreId::B).phase = CorePhase::Seeded;

        checkpoint_state(&path, &state);

        assert!(path.exists(), "Checkpoint file should exist");
        let json = std::fs::read_to_string(&path).unwrap();
        let restored: CoreState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.core_a.estimated_tokens, 50_000);
        assert_eq!(restored.core_a.samples, 100);
        assert_eq!(restored.core_b.phase, CorePhase::Seeded);
    }

    #[test]
    fn checkpoint_is_atomic_via_rename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("core-state.json");
        let tmp_path = path.with_extension("json.tmp");

        let state = CoreState::new(200_000);
        checkpoint_state(&path, &state);

        assert!(path.exists());
        assert!(
            !tmp_path.exists(),
            "Temp file should not persist after atomic rename"
        );
    }
}
