//! Subagent registry — tracks all spawned child agents with lifecycle control.
//!
//! Every subagent gets a purpose-hash name (e.g. `fix-slider-css-a3f9b`) that is:
//! - Human-readable prefix from purpose
//! - Short hash suffix for uniqueness
//! - Addressable by HITL or parent agent
//! - Stable for the subagent's lifetime

use dashmap::DashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Notify;
use tracing::{debug, info};

/// Status of a subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Paused,
    Complete,
    Failed,
    Killed,
}

impl std::fmt::Display for SubagentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Complete => write!(f, "complete"),
            Self::Failed => write!(f, "failed"),
            Self::Killed => write!(f, "killed"),
        }
    }
}

/// Metadata for a tracked subagent.
pub struct SubagentEntry {
    /// The purpose-hash name (e.g. `fix-slider-css-a3f9b`)
    pub name: String,
    /// Original purpose string
    pub purpose: String,
    /// Internal session key used by the runtime
    pub session_id: String,
    /// Current status
    pub status: SubagentStatus,
    /// Token estimate (updated on completion)
    pub tokens: usize,
    /// Wall clock start
    pub started_at: Instant,
    /// Wall clock end (if finished)
    pub ended_at: Option<Instant>,
    /// Last output text (truncated to 500 chars)
    pub last_output: String,
    /// Parent subagent name (None if top-level)
    pub parent: Option<String>,
    /// Children subagent names
    pub children: Vec<String>,
    /// Pause gate — when set, the subagent's LLM loop waits on this notify.
    pub pause_gate: Arc<Notify>,
    /// Whether the pause gate is closed (subagent should wait before next iteration).
    pub is_paused: bool,
    /// Kill signal
    pub kill_requested: bool,
}

/// Generate a purpose-hash name from a purpose string.
///
/// Takes the first few words of the purpose (lowercased, kebab-cased),
/// appends a 5-char hash suffix for uniqueness.
pub fn purpose_hash_name(purpose: &str) -> String {
    // Extract first ~3 meaningful words
    let words: Vec<&str> = purpose
        .split_whitespace()
        .filter(|w| w.len() > 1) // skip tiny words
        .take(4)
        .collect();

    let prefix = if words.is_empty() {
        "agent".to_string()
    } else {
        words
            .iter()
            .map(|w| {
                w.to_lowercase()
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("-")
    };

    // Truncate prefix to 20 chars
    let prefix = if prefix.len() > 20 {
        prefix[..20].to_string()
    } else {
        prefix
    };

    // Hash the full purpose + timestamp for uniqueness
    let mut hasher = DefaultHasher::new();
    purpose.hash(&mut hasher);
    // Add entropy from current time
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    let hash = hasher.finish();
    let suffix = format!("{:05x}", hash & 0xFFFFF); // 5 hex chars

    format!("{}-{}", prefix, suffix)
}

/// Registry of all subagents. Thread-safe, concurrent access.
#[derive(Default)]
pub struct SubagentRegistry {
    agents: DashMap<String, SubagentEntry>,
}

impl SubagentRegistry {
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
        }
    }

    /// Register a new subagent. Returns the purpose-hash name.
    pub fn register(&self, purpose: &str, session_id: &str, parent: Option<&str>) -> String {
        let name = purpose_hash_name(purpose);
        let entry = SubagentEntry {
            name: name.clone(),
            purpose: purpose.to_string(),
            session_id: session_id.to_string(),
            status: SubagentStatus::Running,
            tokens: 0,
            started_at: Instant::now(),
            ended_at: None,
            last_output: String::new(),
            parent: parent.map(String::from),
            children: Vec::new(),
            pause_gate: Arc::new(Notify::new()),
            is_paused: false,
            kill_requested: false,
        };

        // Register with parent
        if let Some(parent_name) = parent {
            if let Some(mut parent_entry) = self.agents.get_mut(parent_name) {
                parent_entry.children.push(name.clone());
            }
        }

        info!(name = %name, purpose = %purpose, session = %session_id, "Subagent registered");
        self.agents.insert(name.clone(), entry);
        name
    }

    /// Mark a subagent as complete with output and token count.
    pub fn mark_complete(&self, name: &str, output: &str, tokens: usize) {
        if let Some(mut entry) = self.agents.get_mut(name) {
            entry.status = SubagentStatus::Complete;
            entry.tokens = tokens;
            entry.ended_at = Some(Instant::now());
            entry.last_output = if output.len() > 500 {
                format!("{}...", &output[..497])
            } else {
                output.to_string()
            };
            info!(name = %name, tokens = tokens, "Subagent completed");
        }
    }

    /// Mark a subagent as failed.
    pub fn mark_failed(&self, name: &str, error: &str) {
        if let Some(mut entry) = self.agents.get_mut(name) {
            entry.status = SubagentStatus::Failed;
            entry.ended_at = Some(Instant::now());
            entry.last_output = format!("ERROR: {}", error);
            info!(name = %name, error = %error, "Subagent failed");
        }
    }

    /// Pause a subagent and all its children (recursive).
    pub fn pause(&self, name: &str) -> Result<(), String> {
        let children = {
            let mut entry = self
                .agents
                .get_mut(name)
                .ok_or_else(|| format!("Subagent '{}' not found", name))?;

            if entry.status != SubagentStatus::Running {
                return Err(format!(
                    "Subagent '{}' is not running (status: {})",
                    name, entry.status
                ));
            }

            entry.is_paused = true;
            entry.status = SubagentStatus::Paused;
            debug!(name = %name, "Subagent paused");
            entry.children.clone()
        };

        // Recursive pause of children
        for child in children {
            let _ = self.pause(&child); // best-effort recursive
        }

        Ok(())
    }

    /// Resume a subagent and all its children (recursive).
    pub fn resume(&self, name: &str) -> Result<(), String> {
        let (gate, children) = {
            let mut entry = self
                .agents
                .get_mut(name)
                .ok_or_else(|| format!("Subagent '{}' not found", name))?;

            if entry.status != SubagentStatus::Paused {
                return Err(format!(
                    "Subagent '{}' is not paused (status: {})",
                    name, entry.status
                ));
            }

            entry.is_paused = false;
            entry.status = SubagentStatus::Running;
            let gate = entry.pause_gate.clone();
            debug!(name = %name, "Subagent resumed");
            (gate, entry.children.clone())
        };

        // Notify the paused loop to continue
        gate.notify_one();

        // Recursive resume of children
        for child in children {
            let _ = self.resume(&child);
        }

        Ok(())
    }

    /// Kill a subagent and all its children (recursive).
    pub fn kill(&self, name: &str) -> Result<(), String> {
        let (gate, children) = {
            let mut entry = self
                .agents
                .get_mut(name)
                .ok_or_else(|| format!("Subagent '{}' not found", name))?;

            match entry.status {
                SubagentStatus::Complete | SubagentStatus::Failed | SubagentStatus::Killed => {
                    return Err(format!(
                        "Subagent '{}' already terminated (status: {})",
                        name, entry.status
                    ));
                }
                _ => {}
            }

            entry.kill_requested = true;
            entry.status = SubagentStatus::Killed;
            entry.ended_at = Some(Instant::now());
            let gate = entry.pause_gate.clone();
            debug!(name = %name, "Subagent killed");
            (gate, entry.children.clone())
        };

        // Wake if paused so it can see the kill flag
        gate.notify_one();

        // Recursive kill of children
        for child in children {
            let _ = self.kill(&child);
        }

        Ok(())
    }

    /// Query a subagent's status.
    pub fn query(&self, name: &str) -> Result<SubagentInfo, String> {
        let entry = self
            .agents
            .get(name)
            .ok_or_else(|| format!("Subagent '{}' not found", name))?;

        Ok(SubagentInfo {
            name: entry.name.clone(),
            purpose: entry.purpose.clone(),
            status: entry.status,
            tokens: entry.tokens,
            elapsed_ms: entry.started_at.elapsed().as_millis() as u64,
            last_output: entry.last_output.clone(),
            children: entry.children.clone(),
            parent: entry.parent.clone(),
        })
    }

    /// List all subagents.
    pub fn list(&self) -> Vec<SubagentInfo> {
        self.agents
            .iter()
            .map(|entry| SubagentInfo {
                name: entry.name.clone(),
                purpose: entry.purpose.clone(),
                status: entry.status,
                tokens: entry.tokens,
                elapsed_ms: entry.started_at.elapsed().as_millis() as u64,
                last_output: entry.last_output.clone(),
                children: entry.children.clone(),
                parent: entry.parent.clone(),
            })
            .collect()
    }

    /// Check if a subagent is paused (should wait before next LLM iteration).
    pub fn is_paused(&self, name: &str) -> bool {
        self.agents.get(name).map(|e| e.is_paused).unwrap_or(false)
    }

    /// Check if a subagent has been killed.
    pub fn is_killed(&self, name: &str) -> bool {
        self.agents
            .get(name)
            .map(|e| e.kill_requested)
            .unwrap_or(false)
    }

    /// Get the pause gate for a subagent (used to wait when paused).
    pub fn pause_gate(&self, name: &str) -> Option<Arc<Notify>> {
        self.agents.get(name).map(|e| e.pause_gate.clone())
    }

    /// Remove completed/failed/killed subagents older than the given duration.
    pub fn gc(&self, max_age: std::time::Duration) {
        let now = Instant::now();
        let to_remove: Vec<String> = self
            .agents
            .iter()
            .filter(|e| {
                matches!(
                    e.status,
                    SubagentStatus::Complete | SubagentStatus::Failed | SubagentStatus::Killed
                ) && e.ended_at.is_some_and(|t| now.duration_since(t) > max_age)
            })
            .map(|e| e.name.clone())
            .collect();

        for name in to_remove {
            self.agents.remove(&name);
        }
    }

    /// Find a subagent by prefix match (for fuzzy addressing).
    pub fn find_by_prefix(&self, prefix: &str) -> Option<String> {
        let prefix_lower = prefix.to_lowercase();
        self.agents
            .iter()
            .find(|e| e.name.starts_with(&prefix_lower))
            .map(|e| e.name.clone())
    }
}

/// Read-only snapshot of a subagent's state.
#[derive(Debug, Clone)]
pub struct SubagentInfo {
    pub name: String,
    pub purpose: String,
    pub status: SubagentStatus,
    pub tokens: usize,
    pub elapsed_ms: u64,
    pub last_output: String,
    pub children: Vec<String>,
    pub parent: Option<String>,
}

impl std::fmt::Display for SubagentInfo {
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

// ---------------------------------------------------------------------------
// SubagentControl trait implementation — bridges agent → tools crate
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl agenticlaw_tools::SubagentControl for SubagentRegistry {
    fn register(&self, purpose: &str, session_id: &str, parent: Option<&str>) -> String {
        SubagentRegistry::register(self, purpose, session_id, parent)
    }

    fn mark_complete(&self, name: &str, output: &str, tokens: usize) {
        SubagentRegistry::mark_complete(self, name, output, tokens)
    }

    fn mark_failed(&self, name: &str, error: &str) {
        SubagentRegistry::mark_failed(self, name, error)
    }

    fn is_paused(&self, name: &str) -> bool {
        SubagentRegistry::is_paused(self, name)
    }

    fn is_killed(&self, name: &str) -> bool {
        SubagentRegistry::is_killed(self, name)
    }

    async fn wait_for_resume(&self, name: &str) {
        if let Some(gate) = self.pause_gate(name) {
            gate.notified().await;
        }
    }

    fn pause(&self, name: &str) -> Result<(), String> {
        SubagentRegistry::pause(self, name)
    }

    fn resume(&self, name: &str) -> Result<(), String> {
        SubagentRegistry::resume(self, name)
    }

    fn kill(&self, name: &str) -> Result<(), String> {
        SubagentRegistry::kill(self, name)
    }

    fn query(&self, name: &str) -> Result<agenticlaw_tools::SubagentInfoSnapshot, String> {
        SubagentRegistry::query(self, name).map(|info| agenticlaw_tools::SubagentInfoSnapshot {
            name: info.name,
            purpose: info.purpose,
            status: info.status.to_string(),
            tokens: info.tokens,
            elapsed_ms: info.elapsed_ms,
            last_output: info.last_output,
            children: info.children,
            parent: info.parent,
        })
    }

    fn list_all(&self) -> Vec<agenticlaw_tools::SubagentInfoSnapshot> {
        SubagentRegistry::list(self)
            .into_iter()
            .map(|info| agenticlaw_tools::SubagentInfoSnapshot {
                name: info.name,
                purpose: info.purpose,
                status: info.status.to_string(),
                tokens: info.tokens,
                elapsed_ms: info.elapsed_ms,
                last_output: info.last_output,
                children: info.children,
                parent: info.parent,
            })
            .collect()
    }

    fn find_by_prefix(&self, prefix: &str) -> Option<String> {
        SubagentRegistry::find_by_prefix(self, prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_purpose_hash_name_format() {
        let name = purpose_hash_name("Fix slider CSS bug in dashboard");
        // Should have kebab-case prefix + 5-char hex suffix
        assert!(name.contains('-'));
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        assert_eq!(parts[0].len(), 5);
        assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_purpose_hash_uniqueness() {
        // Same purpose should produce different names (time-based entropy)
        let name1 = purpose_hash_name("Fix slider CSS");
        std::thread::sleep(std::time::Duration::from_millis(1));
        let name2 = purpose_hash_name("Fix slider CSS");
        assert_ne!(name1, name2);
    }

    #[test]
    fn test_purpose_hash_truncation() {
        let name =
            purpose_hash_name("This is a very long purpose that exceeds twenty characters by far");
        // prefix should be at most 20 chars + dash + 5 hex = max 26 total
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        assert!(parts[1].len() <= 20);
    }

    #[test]
    fn test_registry_register_and_query() {
        let registry = SubagentRegistry::new();
        let name = registry.register("Fix slider CSS", "session-1", None);

        let info = registry.query(&name).unwrap();
        assert_eq!(info.purpose, "Fix slider CSS");
        assert_eq!(info.status, SubagentStatus::Running);
        assert!(info.children.is_empty());
    }

    #[test]
    fn test_registry_pause_resume() {
        let registry = SubagentRegistry::new();
        let name = registry.register("Fix slider", "session-1", None);

        assert!(!registry.is_paused(&name));
        registry.pause(&name).unwrap();
        assert!(registry.is_paused(&name));
        assert_eq!(
            registry.query(&name).unwrap().status,
            SubagentStatus::Paused
        );

        registry.resume(&name).unwrap();
        assert!(!registry.is_paused(&name));
        assert_eq!(
            registry.query(&name).unwrap().status,
            SubagentStatus::Running
        );
    }

    #[test]
    fn test_registry_kill() {
        let registry = SubagentRegistry::new();
        let name = registry.register("Fix slider", "session-1", None);

        registry.kill(&name).unwrap();
        assert!(registry.is_killed(&name));
        assert_eq!(
            registry.query(&name).unwrap().status,
            SubagentStatus::Killed
        );
    }

    #[test]
    fn test_recursive_pause() {
        let registry = SubagentRegistry::new();
        let parent = registry.register("Parent task", "session-1", None);
        let child = registry.register("Child task", "session-2", Some(&parent));

        registry.pause(&parent).unwrap();
        assert!(registry.is_paused(&parent));
        assert!(registry.is_paused(&child));
    }

    #[test]
    fn test_registry_list() {
        let registry = SubagentRegistry::new();
        registry.register("Task A", "session-1", None);
        registry.register("Task B", "session-2", None);

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_mark_complete() {
        let registry = SubagentRegistry::new();
        let name = registry.register("Fix bug", "session-1", None);

        registry.mark_complete(&name, "Fixed the bug successfully", 1500);
        let info = registry.query(&name).unwrap();
        assert_eq!(info.status, SubagentStatus::Complete);
        assert_eq!(info.tokens, 1500);
    }

    #[test]
    fn test_find_by_prefix() {
        let registry = SubagentRegistry::new();
        let name = registry.register("Fix slider CSS", "session-1", None);

        let found = registry.find_by_prefix("fix-slider");
        assert!(found.is_some());
        assert_eq!(found.unwrap(), name);
    }
}
