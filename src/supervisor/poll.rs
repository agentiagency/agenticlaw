use chrono::Utc;
use tokio::sync::mpsc;

use super::conductor;
use super::detect;
use super::lifecycle;
use super::log;
use super::state::{self, extract_context_pct, extract_frontier_keywords, extract_recent_commands};
use super::tmux;
use super::types::*;

/// Check if the frontier summary has keyword overlap with the card description.
/// Simple bag-of-words: if any significant word from the card appears in the frontier, it overlaps.
fn frontier_overlaps(card: &str, frontier: &str) -> bool {
    let stop_words = ["the", "a", "an", "is", "in", "on", "to", "for", "of", "and", "or", "with"];
    let frontier_lower = frontier.to_lowercase();
    card.to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() > 2 && !stop_words.contains(w))
        .any(|word| frontier_lower.contains(&word))
}

pub async fn run_supervisor(config: SupervisorConfig) {
    let mut sv = SupervisorState::new(config.backoff.base_ms);

    // Load persisted card map
    sv.card_map = state::read_card_map(&config.molts_base).await;

    // Conductor command channel
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ConductorCommand>(32);
    tokio::spawn(conductor::listen_stdin(cmd_tx));

    log::info("supervisor_started", serde_json::json!({
        "molts_base": config.molts_base,
        "poll_base_ms": config.backoff.base_ms,
        "poll_max_ms": config.backoff.max_ms,
        "dry_run": config.dry_run,
    }));

    loop {
        // Process any pending conductor commands (non-blocking)
        while let Ok(cmd) = cmd_rx.try_recv() {
            handle_conductor_cmd(&cmd, &mut sv, &config).await;
        }

        // Discover sessions (only ws2-* worker sessions)
        let all_sessions = tmux::list_sessions().await.unwrap_or_default();
        let worker_sessions: Vec<String> = all_sessions
            .into_iter()
            .filter(|s| s.starts_with("ws2-"))
            .collect();

        // Snapshot all workers
        let mut any_change = false;
        for name in &worker_sessions {
            let snap = tmux::snapshot(name, 200).await;

            let session_state = sv
                .sessions
                .entry(name.clone())
                .or_insert_with(|| SessionState::new(name.clone()));

            // Assign card from map if known
            if session_state.card.is_none() {
                session_state.card = sv.card_map.get(name).cloned();
            }

            let prev_unchanged = session_state.consecutive_unchanged;
            session_state.update_from_snapshot(&snap);

            if session_state.consecutive_unchanged == 0 && prev_unchanged > 0 {
                any_change = true;
            }

            // Extract commands and status bar info
            session_state.recent_commands = extract_recent_commands(&snap.pane_content, 10);
            session_state.context_pct = extract_context_pct(&snap.pane_content);
            let new_frontier = extract_frontier_keywords(&snap.pane_content);

            // Track frontier drift: if pane is changing but frontier summary
            // doesn't overlap with the assigned card, increment off-frontier counter
            if session_state.consecutive_unchanged == 0 {
                let drifted = match (&session_state.card, &new_frontier) {
                    (Some(card), Some(frontier)) => !frontier_overlaps(card, frontier),
                    _ => false, // no card assigned = can't detect drift
                };
                if drifted {
                    session_state.cycles_off_frontier += 1;
                } else {
                    session_state.cycles_off_frontier = 0;
                }
            }

            session_state.prev_frontier_summary = session_state.frontier_summary.take();
            session_state.frontier_summary = new_frontier;

            // Detect failures
            let failures = detect::detect_all(
                session_state,
                &snap,
                sv.current_backoff_ms,
                config.frozen_threshold_secs,
            );

            for f in &failures {
                lifecycle::handle_failure(f, config.dry_run).await;
            }

            // Update status based on detection
            if failures.is_empty() {
                if session_state.consecutive_unchanged > 0 {
                    session_state.status = SessionStatus::Idle;
                } else {
                    session_state.status = SessionStatus::Active;
                }
            } else {
                // Use the most severe failure as status
                for f in &failures {
                    match f {
                        FailureMode::Dead { .. } => session_state.status = SessionStatus::Dead,
                        FailureMode::Deranged { .. } => {
                            session_state.status = SessionStatus::Deranged
                        }
                        FailureMode::Frozen { .. } => session_state.status = SessionStatus::Frozen,
                        FailureMode::InfiniteLoop { .. } => {
                            session_state.status = SessionStatus::InfiniteLoop
                        }
                        FailureMode::RabbitHoling { .. } => {
                            session_state.status = SessionStatus::RabbitHoling
                        }
                        FailureMode::SilentStall { .. } => {
                            session_state.status = SessionStatus::Idle
                        }
                    }
                }
            }
        }

        // Remove sessions that no longer exist
        sv.sessions.retain(|name, s| {
            worker_sessions.contains(name) || s.status == SessionStatus::Dead
        });

        // Write state
        sv.last_poll_at = Some(Utc::now());
        if let Err(e) = state::write_in_process(&config.molts_base, &sv.sessions).await {
            log::error("write_error", serde_json::json!({"error": e}));
        }

        // Cycle summary
        let status_counts: std::collections::HashMap<String, usize> = {
            let mut m = std::collections::HashMap::new();
            for s in sv.sessions.values() {
                *m.entry(format!("{}", s.status)).or_insert(0) += 1;
            }
            m
        };
        log::info("poll_cycle", serde_json::json!({
            "sessions": sv.sessions.len(),
            "backoff_ms": sv.current_backoff_ms,
            "any_change": any_change,
            "statuses": status_counts,
        }));

        // Optional: emit full status on stdout for conductor piping
        if config.json_stdout {
            let report: Vec<serde_json::Value> = sv
                .sessions
                .values()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "status": format!("{}", s.status),
                        "card": s.card,
                        "context_pct": s.context_pct,
                        "consecutive_unchanged": s.consecutive_unchanged,
                        "cycles_off_frontier": s.cycles_off_frontier,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        }

        // Adaptive backoff
        if any_change {
            sv.current_backoff_ms = config.backoff.base_ms;
        } else {
            sv.current_backoff_ms =
                ((sv.current_backoff_ms as f64 * config.backoff.multiplier) as u64)
                    .min(config.backoff.max_ms);
        }

        tokio::time::sleep(std::time::Duration::from_millis(sv.current_backoff_ms)).await;
    }
}

async fn handle_conductor_cmd(
    cmd: &ConductorCommand,
    sv: &mut SupervisorState,
    config: &SupervisorConfig,
) {
    match cmd {
        ConductorCommand::SpawnWorker {
            name,
            card,
            briefing,
        } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "spawn_worker", "name": name}));
            if let Some(c) = card {
                sv.card_map.insert(name.clone(), c.clone());
                let _ = state::write_card_map(&config.molts_base, &sv.card_map).await;
            }
            if !config.dry_run {
                let _ =
                    lifecycle::spawn_worker(name, card.as_deref(), briefing.as_deref()).await;
            }
        }
        ConductorCommand::KillWorker { name } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "kill_worker", "name": name}));
            if !config.dry_run {
                let _ = lifecycle::harvest_worker(name, &config.molts_base).await;
                let _ = tmux::kill_session(name).await;
            }
            sv.sessions.remove(name);
        }
        ConductorCommand::StatusReport => {
            let report: Vec<serde_json::Value> = sv
                .sessions
                .values()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "status": format!("{}", s.status),
                        "card": s.card,
                        "context_pct": s.context_pct,
                        "consecutive_unchanged": s.consecutive_unchanged,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        }
        ConductorCommand::SendToWorker { name, keys } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "send_to_worker", "name": name}));
            if !config.dry_run {
                let _ = tmux::send_keys(name, keys).await;
            }
        }
        ConductorCommand::ReassignCard {
            name,
            card,
            briefing,
        } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "reassign_card", "name": name, "card": card}));
            sv.card_map.insert(name.clone(), card.clone());
            let _ = state::write_card_map(&config.molts_base, &sv.card_map).await;
            if let Some(s) = sv.sessions.get_mut(name) {
                s.card = Some(card.clone());
                s.cycles_off_frontier = 0;
            }
            if !config.dry_run {
                if let Some(brief) = briefing {
                    let _ = tmux::send_keys(name, brief).await;
                }
            }
        }
        ConductorCommand::RotateWorker { name, briefing } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "rotate_worker", "name": name}));
            if !config.dry_run {
                // Harvest current state
                let _ = lifecycle::harvest_worker(name, &config.molts_base).await;
                // Kill and respawn
                let _ = tmux::kill_session(name).await;
                let card = sv.card_map.get(name).cloned();
                let _ = lifecycle::spawn_worker(
                    name,
                    card.as_deref(),
                    briefing.as_deref(),
                )
                .await;
            }
            // Reset state
            sv.sessions.insert(name.clone(), SessionState::new(name.clone()));
            if let Some(card) = sv.card_map.get(name).cloned() {
                sv.sessions.get_mut(name).unwrap().card = Some(card);
            }
        }
        ConductorCommand::ContextReset { name, briefing } => {
            log::info("conductor_cmd", serde_json::json!({"cmd": "context_reset", "name": name}));
            if !config.dry_run {
                // Send /clear to claude, then re-brief
                let _ = tmux::send_keys(name, "/clear").await;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Some(brief) = briefing {
                    let _ = tmux::send_keys(name, brief).await;
                }
            }
            if let Some(s) = sv.sessions.get_mut(name) {
                s.consecutive_unchanged = 0;
                s.cycles_off_frontier = 0;
                s.retry_ops.clear();
                s.recent_commands.clear();
            }
        }
        ConductorCommand::ListWorkers => {
            let names: Vec<&String> = sv.sessions.keys().collect();
            println!("{}", serde_json::to_string(&names).unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontier_overlaps_matching() {
        assert!(frontier_overlaps("implement authentication", "working on auth implementation"));
    }

    #[test]
    fn frontier_overlaps_no_match() {
        assert!(!frontier_overlaps("implement authentication", "refactoring database schema"));
    }

    #[test]
    fn frontier_overlaps_ignores_stop_words() {
        // "the" and "for" are stop words, shouldn't match
        assert!(!frontier_overlaps("the for", "the quick fox for dinner"));
    }

    #[test]
    fn frontier_overlaps_case_insensitive() {
        assert!(frontier_overlaps("Deploy Terraform", "running terraform plan"));
    }

    #[test]
    fn conductor_parse_spawn() {
        let json = r#"{"cmd":"spawn_worker","name":"ws2-1","card":"auth","briefing":"implement login"}"#;
        let cmd: ConductorCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ConductorCommand::SpawnWorker { name, card, briefing } => {
                assert_eq!(name, "ws2-1");
                assert_eq!(card.unwrap(), "auth");
                assert_eq!(briefing.unwrap(), "implement login");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn conductor_parse_rotate() {
        let json = r#"{"cmd":"rotate_worker","name":"ws2-3"}"#;
        let cmd: ConductorCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ConductorCommand::RotateWorker { name, briefing } => {
                assert_eq!(name, "ws2-3");
                assert!(briefing.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn conductor_parse_context_reset() {
        let json = r#"{"cmd":"context_reset","name":"ws2-5","briefing":"resume from checkpoint"}"#;
        let cmd: ConductorCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ConductorCommand::ContextReset { name, briefing } => {
                assert_eq!(name, "ws2-5");
                assert_eq!(briefing.unwrap(), "resume from checkpoint");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn conductor_parse_status_report() {
        let json = r#"{"cmd":"status_report"}"#;
        let cmd: ConductorCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, ConductorCommand::StatusReport));
    }
}
