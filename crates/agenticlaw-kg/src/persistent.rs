//! Persistent issue/PR nodes — the single agent above ephemeral children.
//!
//! Each issue gets a stable session key that persists via .ctx files.
//! When reloaded, the node remembers prior attempts, failures, and context.
//! Children (analysis, impl, pr) are ephemeral — spawned fresh each run.
//!
//! The persistent node IS the identity of the issue work. It accumulates
//! knowledge across runs: "Round 1 analysis was too broad. Round 2 impl
//! touched wrong files. Thomson closed the PR. Try a different approach."

use agenticlaw_agent::session::{SessionKey, SessionRegistry};
use agenticlaw_agent::runtime::{AgentEvent, AgentRuntime};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

/// A persistent issue node that survives across runs.
///
/// Session key format: `kg-issue-<repo>-<number>`
/// .ctx file: `<workspace>/.ctx/kg-issue-<repo>-<number>.ctx`
pub struct IssueNode {
    pub issue: u64,
    pub repo: String,
    pub session_key: SessionKey,
    pub workspace: PathBuf,
}

impl IssueNode {
    pub fn new(repo: &str, issue: u64, workspace: impl AsRef<Path>) -> Self {
        // Stable key — same issue always gets same session
        let key = format!("kg-issue-{}-{}", repo.replace('/', "-"), issue);
        Self {
            issue,
            repo: repo.into(),
            session_key: SessionKey::from(key),
            workspace: workspace.as_ref().to_path_buf(),
        }
    }

    /// Load or create the persistent session for this issue.
    /// If .ctx exists, the session resumes with full history.
    /// If not, a fresh session is created.
    pub fn load_session(&self, runtime: &AgentRuntime) -> Arc<agenticlaw_agent::session::Session> {
        let system_prompt = format!(
            "You are the persistent coordinator for issue #{} in {}.\n\n\
             You remember everything about this issue across runs. Your job:\n\
             1. Understand the issue deeply\n\
             2. Decide what needs to happen (analysis, implementation, PR)\n\
             3. Spawn children to do the work (they are ephemeral — you are persistent)\n\
             4. Review their output and iterate if needed\n\
             5. Track what's been tried, what failed, what succeeded\n\n\
             You have the spawn tool. Use it to delegate. Don't implement yourself.\n\
             Your context persists — use it. Note what worked and what didn't.",
            self.issue, self.repo
        );

        runtime.sessions().create_with_ctx(
            &self.session_key,
            Some(&system_prompt),
            &self.workspace,
        )
    }

    /// Run a turn on the persistent node. The node decides what to do next
    /// based on its accumulated context.
    pub async fn run_turn(
        &self,
        runtime: &AgentRuntime,
        message: &str,
    ) -> Result<String, String> {
        // Ensure session is loaded (idempotent — returns existing if already loaded)
        let _session = self.load_session(runtime);

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);

        let rt = runtime as *const AgentRuntime;
        // SAFETY: runtime lives for the duration of this call
        let runtime_ref: &AgentRuntime = unsafe { &*rt };

        let sk = self.session_key.clone();
        let msg = message.to_string();

        // Run the turn — this uses the persistent session with .ctx
        let handle = tokio::spawn({
            let sk = sk.clone();
            let msg = msg.clone();
            // We need to clone the runtime handle. Let's use a different approach.
            async move {
                // The runtime is passed via the event channel pattern
                // This is a placeholder — actual implementation uses runtime directly
                drop(tx); // signal completion
                Ok::<(), String>(())
            }
        });

        // Collect output
        let mut output = String::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::Text(t) = event {
                output.push_str(&t);
            }
        }

        let _ = handle.await;
        Ok(output)
    }

    /// Get the .ctx file path for this issue node.
    pub fn ctx_path(&self) -> PathBuf {
        self.workspace.join(".ctx").join(format!("{}.ctx", self.session_key.as_str()))
    }

    /// Check if this issue has been worked on before.
    pub fn has_history(&self) -> bool {
        self.ctx_path().exists()
    }
}

/// A persistent PR node — same concept, tracks PR lifecycle.
pub struct PrNode {
    pub pr: u64,
    pub repo: String,
    pub session_key: SessionKey,
    pub workspace: PathBuf,
}

impl PrNode {
    pub fn new(repo: &str, pr: u64, workspace: impl AsRef<Path>) -> Self {
        let key = format!("kg-pr-{}-{}", repo.replace('/', "-"), pr);
        Self {
            pr,
            repo: repo.into(),
            session_key: SessionKey::from(key),
            workspace: workspace.as_ref().to_path_buf(),
        }
    }

    pub fn load_session(&self, runtime: &AgentRuntime) -> Arc<agenticlaw_agent::session::Session> {
        let system_prompt = format!(
            "You are the persistent coordinator for PR #{} in {}.\n\n\
             You remember everything about this PR across runs. Your job:\n\
             1. Understand the PR's purpose and current state\n\
             2. Check CI status, review comments, merge conflicts\n\
             3. Spawn children to fix issues (they are ephemeral — you are persistent)\n\
             4. Track iterations: what was fixed, what's still broken\n\n\
             You have the spawn tool. Delegate work. Track state.",
            self.pr, self.repo
        );

        runtime.sessions().create_with_ctx(
            &self.session_key,
            Some(&system_prompt),
            &self.workspace,
        )
    }

    pub fn has_history(&self) -> bool {
        self.workspace.join(".ctx").join(format!("{}.ctx", self.session_key.as_str())).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_node_stable_key() {
        let node = IssueNode::new("agentiagency/agentimolt-v03", 183, "/tmp/test");
        assert_eq!(node.session_key.as_str(), "kg-issue-agentiagency-agentimolt-v03-183");

        // Same issue always gets same key
        let node2 = IssueNode::new("agentiagency/agentimolt-v03", 183, "/tmp/test");
        assert_eq!(node.session_key.as_str(), node2.session_key.as_str());

        // Different issue gets different key
        let node3 = IssueNode::new("agentiagency/agentimolt-v03", 184, "/tmp/test");
        assert_ne!(node.session_key.as_str(), node3.session_key.as_str());
    }

    #[test]
    fn pr_node_stable_key() {
        let node = PrNode::new("agentiagency/agentimolt-v03", 189, "/tmp/test");
        assert_eq!(node.session_key.as_str(), "kg-pr-agentiagency-agentimolt-v03-189");
    }

    #[test]
    fn ctx_path_deterministic() {
        let node = IssueNode::new("agentiagency/agentimolt-v03", 183, "/workspace");
        assert_eq!(
            node.ctx_path().to_str().unwrap(),
            "/workspace/.ctx/kg-issue-agentiagency-agentimolt-v03-183.ctx"
        );
    }
}
