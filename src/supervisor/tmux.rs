use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use chrono::Utc;
use tokio::process::Command;

use super::types::SessionSnapshot;

async fn run_tmux(args: &[&str]) -> Result<String, String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("tmux exec failed: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("tmux error: {stderr}"))
    }
}

pub async fn session_exists(name: &str) -> bool {
    run_tmux(&["has-session", "-t", name]).await.is_ok()
}

pub async fn capture_pane(name: &str, lines: u32) -> Result<String, String> {
    run_tmux(&["capture-pane", "-t", name, "-p", "-S", &format!("-{lines}")]).await
}

pub async fn send_keys(name: &str, keys: &str) -> Result<(), String> {
    run_tmux(&["send-keys", "-t", name, keys, "Enter"])
        .await
        .map(|_| ())
}

pub async fn new_session(name: &str, cwd: Option<&str>) -> Result<(), String> {
    let mut args = vec!["new-session", "-d", "-s", name];
    if let Some(dir) = cwd {
        args.extend_from_slice(&["-c", dir]);
    }
    run_tmux(&args).await.map(|_| ())
}

pub async fn kill_session(name: &str) -> Result<(), String> {
    run_tmux(&["kill-session", "-t", name]).await.map(|_| ())
}

pub async fn list_sessions() -> Result<Vec<String>, String> {
    match run_tmux(&["list-sessions", "-F", "#{session_name}"]).await {
        Ok(output) => Ok(output.lines().map(String::from).collect()),
        Err(e) if e.contains("no server running") => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub async fn snapshot(name: &str, lines: u32) -> SessionSnapshot {
    let exists = session_exists(name).await;
    let pane_content = if exists {
        capture_pane(name, lines).await.unwrap_or_default()
    } else {
        String::new()
    };

    let mut hasher = DefaultHasher::new();
    pane_content.trim().hash(&mut hasher);
    let pane_hash = hasher.finish();

    SessionSnapshot {
        name: name.to_string(),
        exists,
        pane_content,
        pane_hash,
        captured_at: Utc::now(),
    }
}
