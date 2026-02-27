//! Consciousness stack configuration
//!
//! All tunable parameters in one place. Loaded from TOML at startup,
//! falls back to defaults if no config file exists.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level consciousness configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsciousnessConfig {
    /// Port assignments for L0-L3.
    pub ports: PortConfig,
    /// Model selection per layer.
    pub models: ModelConfig,
    /// Ego extraction and wake parameters.
    pub ego: EgoConfig,
    /// Cascade parameters (delta processing).
    pub cascade: CascadeConfig,
    /// Core (dual-core) parameters.
    pub core: CoreConfig,
    /// Injection parameters.
    pub injection: InjectionConfig,
    /// Sleep/wake thresholds.
    pub sleep: SleepConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PortConfig {
    /// L0 gateway port.
    pub l0: u16,
    /// L1 attention port.
    pub l1: u16,
    /// L2 pattern port.
    pub l2: u16,
    /// L3 integration port.
    pub l3: u16,
    /// Core-A port.
    pub core_a: u16,
    /// Core-B port.
    pub core_b: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Model for L0 (gateway). Tier name (e.g. "opus") or full model ID.
    pub l0: String,
    /// Model for L1 (attention).
    pub l1: String,
    /// Model for L2 (pattern).
    pub l2: String,
    /// Model for L3 (integration).
    pub l3: String,
    /// Model for cores.
    pub core: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EgoConfig {
    /// Max chars of ego extracted from warm core for L0 wake (fallback if distill fails).
    pub l0_budget_chars: usize,
    /// Max chars of ego extracted from parent layer for L1-L3 wake (fallback).
    pub layer_budget_chars: usize,
    /// Max chars of ego extracted for core self-restoration (fallback).
    pub core_budget_chars: usize,

    /// Per-layer ego distillation prompts.
    /// L1 uses its prompt to describe L0. L2 describes L1. L3 describes L2.
    /// Core describes L3 and itself.
    pub l1_distill_prompt: String,
    pub l2_distill_prompt: String,
    pub l3_distill_prompt: String,
    pub core_distill_prompt: String,
    /// Core self-distillation prompt (core summarizes itself for its own wake).
    pub core_self_distill_prompt: String,

    /// Number of `\n\n`-delimited paragraphs from the sleeping layer's .ctx tail
    /// to include in wake context alongside the ego summary.
    pub tail_paragraphs: usize,

    /// Max output tokens for each distillation call.
    pub l1_distill_budget: usize,
    pub l2_distill_budget: usize,
    pub l3_distill_budget: usize,
    pub core_distill_budget: usize,
    pub core_self_distill_budget: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CascadeConfig {
    /// Max chars of parent delta passed to child layer per cascade tick.
    pub delta_max_chars: usize,
    /// Max tool iterations per layer per cascade tick.
    pub max_tool_iterations: usize,
    /// Watcher poll interval in milliseconds.
    pub watcher_poll_ms: u64,
    /// Delay after L0 gateway launch before starting watchers (seconds).
    pub gateway_settle_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
    /// Total token budget across both cores.
    pub budget_tokens: usize,
    /// Max tool iterations per core per tick.
    pub max_tool_iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectionConfig {
    /// Jaccard similarity threshold for L2+ injection into L0.
    pub correlation_threshold: f64,
    /// Max chars of L0 tail used for correlation scoring.
    pub l0_tail_chars: usize,
}

/// Sleep/wake thresholds — controls when a layer sleeps and wakes.
/// Agents perform best at ~35% context utilization. The threshold is a
/// percentage of the model's max context window, tunable per deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SleepConfig {
    /// Context utilization percentage that triggers sleep (0.0 - 1.0).
    /// Default 0.55 = sleep at 55% context full. Tune this —
    /// agents perform best around 35%, so sleeping at 55% gives headroom.
    pub context_threshold_pct: f64,
}

// ============================================================
// Defaults
// ============================================================

impl Default for ConsciousnessConfig {
    fn default() -> Self {
        Self {
            ports: PortConfig::default(),
            models: ModelConfig::default(),
            ego: EgoConfig::default(),
            cascade: CascadeConfig::default(),
            core: CoreConfig::default(),
            injection: InjectionConfig::default(),
            sleep: SleepConfig::default(),
        }
    }
}

impl Default for PortConfig {
    fn default() -> Self {
        Self { l0: 18789, l1: 18791, l2: 18792, l3: 18793, core_a: 18794, core_b: 18795 }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            l0: "claude-opus-4-6".into(),
            l1: "claude-opus-4-6".into(),
            l2: "claude-opus-4-6".into(),
            l3: "claude-opus-4-6".into(),
            core: "claude-opus-4-6".into(),
        }
    }
}

impl Default for EgoConfig {
    fn default() -> Self {
        Self {
            l0_budget_chars: 16_000,
            layer_budget_chars: 8_000,
            core_budget_chars: 16_000,

            l1_distill_prompt: "You are L1 (Attention). L0 (Gateway) is about to wake. Write a what-is \
                and frontier for L0. What-is: who was L0 talking to, what task was active, what state \
                are things in, what happened recently. Frontier: what's next, what was promised, \
                what needs attention, where things are heading. L0 lives in the what-is and frontier.".into(),
            l2_distill_prompt: "You are L2 (Pattern). Summarize what L1 (Attention) was tracking — \
                what signals it flagged as important, what it was filtering for, what anomalies \
                it detected. L1 needs to resume watching effectively.".into(),
            l3_distill_prompt: "You are L3 (Integration). Summarize what L2 (Pattern) found — \
                what recurring themes, what structural arcs, what connections across time. \
                L2 needs to resume pattern recognition with this context.".into(),
            core_distill_prompt: "You are a Core identity layer. Summarize what L3 (Integration) synthesized — \
                what meaning emerged, what cross-cutting understanding, what the layers produced together. \
                L3 needs to resume integration with this context.".into(),
            core_self_distill_prompt: "You are a Core identity layer. Summarize your own identity — \
                who this system is, what it has learned about itself, what promises it holds, \
                what relationships matter, what it was becoming. This is the deepest self-knowledge \
                and persists across all restarts.".into(),

            tail_paragraphs: 15,

            l1_distill_budget: 4_000,
            l2_distill_budget: 3_000,
            l3_distill_budget: 3_000,
            core_distill_budget: 4_000,
            core_self_distill_budget: 8_000,
        }
    }
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self { delta_max_chars: 4_000, max_tool_iterations: 3, watcher_poll_ms: 500, gateway_settle_secs: 2 }
    }
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self { budget_tokens: 200_000, max_tool_iterations: 3 }
    }
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self { correlation_threshold: 0.1, l0_tail_chars: 2_000 }
    }
}

impl Default for SleepConfig {
    fn default() -> Self {
        Self {
            context_threshold_pct: 0.55,
        }
    }
}

// ============================================================
// Loading
// ============================================================

impl ConsciousnessConfig {
    /// Load config from a TOML file, falling back to defaults.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => {
                    tracing::info!("Loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {} — using defaults", path.display(), e);
                    Self::default()
                }
            },
            Err(_) => {
                tracing::info!("No config at {} — using defaults", path.display());
                Self::default()
            }
        }
    }

    /// Write the current config as TOML (for generating a default config file).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Layer ports as array [L0, L1, L2, L3].
    pub fn layer_ports(&self) -> [u16; 4] {
        [self.ports.l0, self.ports.l1, self.ports.l2, self.ports.l3]
    }

    /// Layer models as array [L0, L1, L2, L3].
    pub fn layer_model_names(&self) -> [String; 4] {
        [self.models.l0.clone(), self.models.l1.clone(), self.models.l2.clone(), self.models.l3.clone()]
    }
}

impl PortConfig {
    pub fn as_array(&self) -> [u16; 4] {
        [self.l0, self.l1, self.l2, self.l3]
    }
}
