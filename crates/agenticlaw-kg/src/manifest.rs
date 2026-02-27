//! Run manifest — structured metadata for every KG execution.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunManifest {
    pub run_id: String,
    pub purpose: String,
    pub graph_address: String,
    pub target: String,
    pub started: DateTime<Utc>,
    pub ended: Option<DateTime<Utc>>,
    pub outcome: Outcome,
    pub total_tokens: usize,
    pub total_wall_ms: u64,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub iterations: Vec<Iteration>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pending,
    Success,
    Failure,
    Abandoned,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Success => write!(f, "success"),
            Self::Failure => write!(f, "failure"),
            Self::Abandoned => write!(f, "abandoned"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeStatus {
    pub status: NodeState,
    pub session_key: Option<String>,
    pub tokens: usize,
    pub wall_ms: u64,
    pub started: Option<DateTime<Utc>>,
    pub ended: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NodeState {
    Pending,
    Blocked,
    Spawned,
    Running,
    Success,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Iteration {
    pub node: String,
    pub round: usize,
    pub feedback_summary: String,
    pub ts: DateTime<Utc>,
}

impl RunManifest {
    pub fn new(run_id: &str, purpose: &str, graph_address: &str, target: &str) -> Self {
        Self {
            run_id: run_id.into(),
            purpose: purpose.into(),
            graph_address: graph_address.into(),
            target: target.into(),
            started: Utc::now(),
            ended: None,
            outcome: Outcome::Pending,
            total_tokens: 0,
            total_wall_ms: 0,
            nodes: BTreeMap::new(),
            iterations: Vec::new(),
        }
    }

    pub fn add_node(&mut self, addr: &str, state: NodeState) {
        self.nodes.insert(addr.into(), NodeStatus {
            status: state,
            session_key: None,
            tokens: 0,
            wall_ms: 0,
            started: None,
            ended: None,
        });
    }

    pub fn update_node(&mut self, addr: &str, state: NodeState) {
        if let Some(node) = self.nodes.get_mut(addr) {
            node.status = state;
        }
    }

    pub fn start_node(&mut self, addr: &str, session_key: Option<String>) {
        if let Some(node) = self.nodes.get_mut(addr) {
            node.status = NodeState::Running;
            node.session_key = session_key;
            node.started = Some(Utc::now());
        }
    }

    pub fn finish_node(&mut self, addr: &str, state: NodeState, tokens: usize) {
        if let Some(node) = self.nodes.get_mut(addr) {
            node.status = state;
            node.tokens = tokens;
            node.ended = Some(Utc::now());
            if let (Some(start), Some(end)) = (node.started, node.ended) {
                node.wall_ms = (end - start).num_milliseconds().max(0) as u64;
            }
        }
    }

    pub fn finalize(&mut self, outcome: Outcome) {
        self.outcome = outcome;
        self.ended = Some(Utc::now());
        self.total_tokens = self.nodes.values().map(|n| n.tokens).sum();
        self.total_wall_ms = (self.ended.unwrap() - self.started).num_milliseconds().max(0) as u64;
    }

    pub fn to_yaml(&self) -> String {
        serde_yaml::to_string(self).unwrap_or_default()
    }

    pub fn run_log_line(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} |",
            self.run_id,
            self.purpose,
            self.graph_address,
            self.target,
            self.started.format("%Y-%m-%dT%H:%M:%S"),
            self.outcome,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lifecycle() {
        let mut m = RunManifest::new("183-test", "Fix slider", "/root/issue/183", "issues/183");
        m.add_node("/root/issue/183/analysis", NodeState::Pending);
        m.add_node("/root/issue/183/impl", NodeState::Blocked);
        m.add_node("/root/issue/183/pr", NodeState::Blocked);

        m.start_node("/root/issue/183/analysis", Some("sess-123".into()));
        assert_eq!(m.nodes["/root/issue/183/analysis"].status, NodeState::Running);

        m.finish_node("/root/issue/183/analysis", NodeState::Success, 5000);
        assert_eq!(m.nodes["/root/issue/183/analysis"].tokens, 5000);

        m.finalize(Outcome::Success);
        assert_eq!(m.outcome, Outcome::Success);
        assert_eq!(m.total_tokens, 5000);
    }

    #[test]
    fn manifest_yaml_roundtrip() {
        let m = RunManifest::new("42-test", "Test", "/root/issue/42", "issues/42");
        let yaml = m.to_yaml();
        assert!(yaml.contains("42-test"));
        assert!(yaml.contains("pending"));
    }

    #[test]
    fn run_log_line_format() {
        let m = RunManifest::new("42-test", "Test purpose", "/root/issue/42", "issues/42");
        let line = m.run_log_line();
        assert!(line.contains("42-test"));
        assert!(line.contains("Test purpose"));
        assert!(line.contains("/root/issue/42"));
    }
}
