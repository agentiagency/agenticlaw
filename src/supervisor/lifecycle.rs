use chrono::Utc;
use std::path::Path;
use tokio::fs;

use super::log;
use super::tmux;
use super::types::FailureMode;

pub async fn spawn_worker(
    name: &str,
    card: Option<&str>,
    briefing: Option<&str>,
) -> Result<(), String> {
    tmux::new_session(name, None).await?;
    tmux::send_keys(name, "claude").await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    if let Some(brief) = briefing {
        tmux::send_keys(name, brief).await?;
    }
    log::info(
        "worker_spawned",
        serde_json::json!({
            "session": name, "card": card
        }),
    );
    Ok(())
}

pub async fn harvest_worker(name: &str, base: &str) -> Result<(), String> {
    let content = tmux::capture_pane(name, 500).await?;

    let dir = Path::new(base).join("supervisor").join("harvested");
    fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("mkdir: {e}"))?;

    let ts = Utc::now().format("%Y-%m-%dT%H%M");
    let path = dir.join(format!("{ts}-{name}.txt"));
    fs::write(&path, &content)
        .await
        .map_err(|e| format!("write harvest: {e}"))?;

    log::info(
        "worker_harvested",
        serde_json::json!({
            "session": name, "path": path.display().to_string()
        }),
    );
    Ok(())
}

pub async fn handle_failure(failure: &FailureMode, dry_run: bool) {
    match failure {
        FailureMode::Frozen {
            session,
            unchanged_secs,
        } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "frozen", "session": session, "unchanged_secs": unchanged_secs
                }),
            );
            if !dry_run {
                let _ =
                    tmux::send_keys(session, "# supervisor: you appear frozen, please continue")
                        .await;
            }
        }
        FailureMode::Deranged {
            session,
            repeated_pattern,
            count,
        } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "deranged", "session": session, "repeated_pattern": repeated_pattern, "count": count
                }),
            );
            if !dry_run {
                let _ = tmux::send_keys(
                    session,
                    "# supervisor: you are repeating the same action, try a different approach",
                )
                .await;
            }
        }
        FailureMode::Dead { session } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "dead", "session": session
                }),
            );
        }
        FailureMode::InfiniteLoop {
            session,
            operation,
            retries,
        } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "infinite_loop", "session": session, "operation": operation, "retries": retries
                }),
            );
            if !dry_run {
                let _ = tmux::send_keys(
                    session,
                    &format!("# supervisor: '{operation}' has failed {retries} times, escalating"),
                )
                .await;
            }
        }
        FailureMode::RabbitHoling {
            session,
            cycles_off_frontier,
        } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "rabbit_holing", "session": session, "cycles_off_frontier": cycles_off_frontier
                }),
            );
        }
        FailureMode::SilentStall { session } => {
            log::warn(
                "failure_detected",
                serde_json::json!({
                    "type": "silent_stall", "session": session
                }),
            );
        }
    }
}
