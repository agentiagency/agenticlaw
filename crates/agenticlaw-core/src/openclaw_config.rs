//! OpenClaw config — serde structs for ~/.openclaw/openclaw.json
//!
//! Pure types and parsing only. Watching/hot-reload lives in agenticlaw-agent.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpenclawConfig {
    pub gateway: OcGateway,
    pub agents: OcAgents,
    pub models: OcModels,
    pub tools: OcTools,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcGateway {
    pub port: Option<u16>,
    pub mode: Option<String>,
    pub bind: Option<String>,
    pub auth: OcGatewayAuth,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcGatewayAuth {
    pub mode: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcAgents {
    pub defaults: OcAgentDefaults,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcAgentDefaults {
    pub model: OcModelRef,
    pub workspace: Option<String>,
    #[serde(rename = "contextTokens")]
    pub context_tokens: Option<usize>,
    #[serde(rename = "maxConcurrent")]
    pub max_concurrent: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcModelRef {
    pub primary: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcModels {
    pub providers: OcProviders,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcProviders {
    pub anthropic: Option<OcAnthropicProvider>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcAnthropicProvider {
    #[serde(rename = "baseUrl")]
    pub base_url: Option<String>,
    pub models: Vec<OcModelEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcModelEntry {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "contextWindow")]
    pub context_window: Option<usize>,
    #[serde(rename = "maxTokens")]
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcTools {
    pub web: OcWebTools,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcWebTools {
    pub search: OcSearchConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OcSearchConfig {
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
}

/// Files loaded from workspace at session init, injected into system prompt.
pub const BOOTSTRAP_FILES: &[&str] = &[
    "AGENTS.md",
    "SOUL.md",
    "TOOLS.md",
    "IDENTITY.md",
    "USER.md",
    "HEARTBEAT.md",
    "MEMORY.md",
];

impl OpenclawConfig {
    /// Load from a specific path.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Discover from ~/.openclaw/openclaw.json.
    pub fn discover() -> Self {
        Self::load(&Self::default_path())
    }

    /// Default path: ~/.openclaw/openclaw.json
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".openclaw").join("openclaw.json")
    }

    /// Workspace from config, or ~/.openclaw/workspace
    pub fn workspace(&self) -> PathBuf {
        self.agents
            .defaults
            .workspace
            .as_ref()
            .map(|w| expand_tilde(w))
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".openclaw").join("workspace")
            })
    }

    /// Primary model ID, stripping provider/ prefix.
    pub fn default_model(&self) -> Option<String> {
        self.agents
            .defaults
            .model
            .primary
            .as_ref()
            .map(|m| m.split('/').next_back().unwrap_or(m).to_string())
    }

    pub fn gateway_port(&self) -> Option<u16> {
        self.gateway.port
    }
    pub fn gateway_bind(&self) -> Option<&str> {
        self.gateway.bind.as_deref()
    }
    pub fn gateway_token(&self) -> Option<&str> {
        self.gateway.auth.token.as_deref()
    }
    pub fn gateway_auth_mode(&self) -> Option<&str> {
        self.gateway.auth.mode.as_deref()
    }

    pub fn anthropic_base_url(&self) -> Option<&str> {
        self.models
            .providers
            .anthropic
            .as_ref()
            .and_then(|p| p.base_url.as_deref())
    }

    pub fn context_tokens(&self) -> Option<usize> {
        self.agents.defaults.context_tokens
    }
}

/// Load bootstrap identity files from workspace, walking up to ~ for AGENTS.md.
pub fn load_bootstrap_files(workspace: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for name in BOOTSTRAP_FILES {
        let path = workspace.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                files.push((name.to_string(), content));
            }
        }
    }

    // Walk parents for AGENTS.md (like CLAUDE.md in Claude Code)
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut dir = workspace.parent();
    while let Some(parent) = dir {
        if home
            .as_ref()
            .is_some_and(|h| parent == h.parent().unwrap_or(h))
        {
            break;
        }
        let path = parent.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                files.push((format!("AGENTS.md ({})", path.display()), content));
            }
        }
        dir = parent.parent();
    }

    files
}

/// Format bootstrap files into a system prompt section.
pub fn bootstrap_to_system_prompt(files: &[(String, String)]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut prompt = String::from("# Workspace Context\n\n");
    for (name, content) in files {
        prompt.push_str(&format!("## {}\n\n{}\n\n", name, content.trim()));
    }
    Some(prompt)
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
