//! Resource driver abstraction — pluggable backend for KG executor artifacts.
//!
//! The executor writes to abstract graph addresses. The driver maps them to physical storage.
//! Today: local filesystem. Tomorrow: S3, Loki, database.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// Abstract resource address in the KG execution tree.
/// e.g. "/root/issue/183/analysis"
#[derive(Clone, Debug)]
pub struct GraphAddress(pub String);

impl GraphAddress {
    pub fn root() -> Self {
        Self("/root".into())
    }
    pub fn child(&self, segment: &str) -> Self {
        Self(format!("{}/{}", self.0, segment))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GraphAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Structured event emitted by KG nodes (Loki-compatible).
#[derive(serde::Serialize, Clone, Debug)]
pub struct KgEvent {
    pub ts: DateTime<Utc>,
    pub graph_address: String,
    pub event: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Artifact types written by the executor scaffolding (not the agent).
pub enum Artifact {
    Prompt,     // prompt.md — task given to agent
    Fear,       // fear.md — constraints/warnings
    Ego,        // ego.md — context snapshot
    Context,    // context.md — files read for scoping
    Output,     // output.md — agent's result
    Transcript, // ctx.jsonl — full session transcript
    Decision,   // decision.md — gate approve/reject
    Metrics,    // metrics.yaml — tokens, wall_ms, outcome
    Manifest,   // manifest.yaml — run metadata
    Report,     // report.md — final summary
}

impl Artifact {
    pub fn filename(&self) -> &str {
        match self {
            Self::Prompt => "prompt.md",
            Self::Fear => "fear.md",
            Self::Ego => "ego.md",
            Self::Context => "context.md",
            Self::Output => "output.md",
            Self::Transcript => "ctx.jsonl",
            Self::Decision => "decision.md",
            Self::Metrics => "metrics.yaml",
            Self::Manifest => "manifest.yaml",
            Self::Report => "report.md",
        }
    }
}

/// Pluggable resource driver. Code writes here; backend decides where it goes.
#[async_trait::async_trait]
pub trait ResourceDriver: Send + Sync {
    /// Write an artifact at a graph address.
    async fn write_artifact(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        artifact: Artifact,
        content: &[u8],
    ) -> Result<()>;

    /// Read an artifact back.
    async fn read_artifact(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        artifact: Artifact,
    ) -> Result<Vec<u8>>;

    /// Emit a structured event (for ctx.jsonl / Loki).
    async fn emit_event(&self, run_id: &str, event: KgEvent) -> Result<()>;

    /// Get the physical path/URI for a run (for human inspection).
    fn run_location(&self, run_id: &str) -> String;
}

/// Local filesystem driver — writes to ~/tmp/kg-runs/<run-id>/
pub struct LocalFsDriver {
    base_dir: PathBuf,
}

impl LocalFsDriver {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    fn artifact_path(&self, run_id: &str, addr: &GraphAddress, artifact: &Artifact) -> PathBuf {
        // /root/issue/183/analysis → issue-183/analysis/
        let rel = addr.0.trim_start_matches("/root").trim_start_matches('/');
        let rel = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
        self.base_dir
            .join(run_id)
            .join(if rel.is_empty() { "root" } else { &rel })
            .join(artifact.filename())
    }
}

#[async_trait::async_trait]
impl ResourceDriver for LocalFsDriver {
    async fn write_artifact(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        artifact: Artifact,
        content: &[u8],
    ) -> Result<()> {
        let path = self.artifact_path(run_id, addr, &artifact);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        tracing::debug!("wrote {} ({} bytes)", path.display(), content.len());
        Ok(())
    }

    async fn read_artifact(
        &self,
        run_id: &str,
        addr: &GraphAddress,
        artifact: Artifact,
    ) -> Result<Vec<u8>> {
        let path = self.artifact_path(run_id, addr, &artifact);
        Ok(tokio::fs::read(&path).await?)
    }

    async fn emit_event(&self, run_id: &str, event: KgEvent) -> Result<()> {
        let path = self.artifact_path(
            run_id,
            &GraphAddress(event.graph_address.clone()),
            &Artifact::Transcript,
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');
        // Append mode
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }

    fn run_location(&self, run_id: &str) -> String {
        self.base_dir.join(run_id).display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_address_child() {
        let root = GraphAddress::root();
        let issue = root.child("issue").child("183");
        assert_eq!(issue.as_str(), "/root/issue/183");
        let analysis = issue.child("analysis");
        assert_eq!(analysis.as_str(), "/root/issue/183/analysis");
    }

    #[tokio::test]
    async fn local_fs_write_read() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = LocalFsDriver::new(tmp.path());
        let addr = GraphAddress("/root/issue/42/analysis".into());

        driver
            .write_artifact("test-run", &addr, Artifact::Prompt, b"hello world")
            .await
            .unwrap();
        let read_back = driver
            .read_artifact("test-run", &addr, Artifact::Prompt)
            .await
            .unwrap();
        assert_eq!(read_back, b"hello world");
    }

    #[tokio::test]
    async fn local_fs_emit_events() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = LocalFsDriver::new(tmp.path());
        let event = KgEvent {
            ts: Utc::now(),
            graph_address: "/root/issue/42/analysis".into(),
            event: "node_spawned".into(),
            data: serde_json::json!({"tools": ["bash", "read"]}),
        };
        driver.emit_event("test-run", event).await.unwrap();
        let content = driver
            .read_artifact(
                "test-run",
                &GraphAddress("/root/issue/42/analysis".into()),
                Artifact::Transcript,
            )
            .await
            .unwrap();
        let line = String::from_utf8(content).unwrap();
        assert!(line.contains("node_spawned"));
    }
}
