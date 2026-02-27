use super::types::{FailureMode, SessionSnapshot, SessionState};

pub fn detect_frozen(
    state: &SessionState,
    snapshot: &SessionSnapshot,
    interval_ms: u64,
    threshold_secs: u64,
) -> Option<FailureMode> {
    if !snapshot.exists {
        return None;
    }
    let unchanged_secs = state.consecutive_unchanged as u64 * interval_ms / 1000;
    if unchanged_secs >= threshold_secs {
        Some(FailureMode::Frozen {
            session: state.name.clone(),
            unchanged_secs,
        })
    } else {
        None
    }
}

pub fn detect_deranged(state: &SessionState, snapshot: &SessionSnapshot) -> Option<FailureMode> {
    if !snapshot.exists || state.recent_commands.len() < 3 {
        return None;
    }

    // Check for repeated identical commands in the last N
    let last = &state.recent_commands;
    let len = last.len();
    if len >= 3 {
        let tail = &last[len - 3..];
        if tail[0] == tail[1] && tail[1] == tail[2] && !tail[0].is_empty() {
            return Some(FailureMode::Deranged {
                session: state.name.clone(),
                repeated_pattern: tail[0].clone(),
                count: 3,
            });
        }
    }

    // Check for repeated error patterns in pane content
    let error_lines: Vec<&str> = snapshot
        .pane_content
        .lines()
        .filter(|l| l.contains("Error") || l.contains("error:") || l.contains("FAILED"))
        .collect();

    if error_lines.len() >= 3 {
        let last_err = error_lines.last().unwrap();
        let repeats = error_lines.iter().filter(|l| *l == last_err).count();
        if repeats >= 3 {
            return Some(FailureMode::Deranged {
                session: state.name.clone(),
                repeated_pattern: last_err.to_string(),
                count: repeats as u32,
            });
        }
    }

    None
}

pub fn detect_dead(snapshot: &SessionSnapshot) -> Option<FailureMode> {
    if !snapshot.exists {
        Some(FailureMode::Dead {
            session: snapshot.name.clone(),
        })
    } else {
        None
    }
}

pub fn detect_infinite_loop(state: &SessionState) -> Option<FailureMode> {
    for (op, count) in &state.retry_ops {
        if *count > 3 {
            return Some(FailureMode::InfiniteLoop {
                session: state.name.clone(),
                operation: op.clone(),
                retries: *count,
            });
        }
    }
    None
}

/// Detects rabbit-holing: session is active but working off-frontier for multiple cycles.
/// The poll loop tracks `cycles_off_frontier` by comparing frontier summaries across cycles.
/// If the worker has been off-frontier for >2 cycles, flag it.
pub fn detect_rabbit_holing(
    state: &SessionState,
    _snapshot: &SessionSnapshot,
) -> Option<FailureMode> {
    if state.cycles_off_frontier > 2 {
        Some(FailureMode::RabbitHoling {
            session: state.name.clone(),
            cycles_off_frontier: state.cycles_off_frontier,
        })
    } else {
        None
    }
}

/// Detects silent stall: session exists, pane shows a prompt, but nothing is happening.
/// Different from frozen: frozen means pane content is unchanged. Silent stall means
/// the worker is at a prompt (waiting for input) rather than executing.
pub fn detect_silent_stall(
    state: &SessionState,
    snapshot: &SessionSnapshot,
) -> Option<FailureMode> {
    if !snapshot.exists {
        return None;
    }

    // Only trigger if unchanged for at least 2 cycles (not just a brief pause)
    if state.consecutive_unchanged < 2 {
        return None;
    }

    // Look for prompt indicators in the last few lines
    let tail: Vec<&str> = snapshot.pane_content.lines().rev().take(3).collect();
    let at_prompt = tail.iter().any(|line| {
        let trimmed = line.trim();
        trimmed.ends_with('$')
            || trimmed.ends_with('>')
            || trimmed.ends_with('%')
            || trimmed.ends_with(">>>")
    });

    if at_prompt {
        Some(FailureMode::SilentStall {
            session: state.name.clone(),
        })
    } else {
        None
    }
}

pub fn detect_all(
    state: &SessionState,
    snapshot: &SessionSnapshot,
    interval_ms: u64,
    frozen_threshold_secs: u64,
) -> Vec<FailureMode> {
    let mut failures = Vec::new();

    if let Some(f) = detect_dead(snapshot) {
        failures.push(f);
        return failures; // dead trumps all
    }

    if let Some(f) = detect_frozen(state, snapshot, interval_ms, frozen_threshold_secs) {
        failures.push(f);
    }
    if let Some(f) = detect_deranged(state, snapshot) {
        failures.push(f);
    }
    if let Some(f) = detect_infinite_loop(state) {
        failures.push(f);
    }
    if let Some(f) = detect_silent_stall(state, snapshot) {
        failures.push(f);
    }
    if let Some(f) = detect_rabbit_holing(state, snapshot) {
        failures.push(f);
    }

    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_state(name: &str, unchanged: u32) -> SessionState {
        let mut s = SessionState::new(name.to_string());
        s.consecutive_unchanged = unchanged;
        s
    }

    fn make_snapshot(name: &str, exists: bool, content: &str) -> SessionSnapshot {
        SessionSnapshot {
            name: name.to_string(),
            exists,
            pane_content: content.to_string(),
            pane_hash: 0,
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn frozen_detected_at_threshold() {
        let state = make_state("w1", 30); // 30 * 8000ms = 240s
        let snap = make_snapshot("w1", true, "some output");
        let result = detect_frozen(&state, &snap, 8000, 240);
        assert!(result.is_some());
    }

    #[test]
    fn frozen_not_detected_below_threshold() {
        let state = make_state("w1", 29);
        let snap = make_snapshot("w1", true, "some output");
        let result = detect_frozen(&state, &snap, 8000, 240);
        assert!(result.is_none());
    }

    #[test]
    fn dead_detected_when_session_gone() {
        let snap = make_snapshot("w1", false, "");
        assert!(detect_dead(&snap).is_some());
    }

    #[test]
    fn deranged_detected_on_repeated_commands() {
        let mut state = make_state("w1", 0);
        state.recent_commands = vec![
            "cargo build".into(),
            "cargo build".into(),
            "cargo build".into(),
        ];
        let snap = make_snapshot("w1", true, "");
        assert!(detect_deranged(&state, &snap).is_some());
    }

    #[test]
    fn deranged_not_detected_varied_commands() {
        let mut state = make_state("w1", 0);
        state.recent_commands = vec![
            "cargo build".into(),
            "cargo test".into(),
            "cargo run".into(),
        ];
        let snap = make_snapshot("w1", true, "");
        assert!(detect_deranged(&state, &snap).is_none());
    }

    #[test]
    fn infinite_loop_detected() {
        let mut state = make_state("w1", 0);
        state.retry_ops.insert("install_dep".into(), 4);
        assert!(detect_infinite_loop(&state).is_some());
    }

    #[test]
    fn silent_stall_at_prompt() {
        let state = make_state("w1", 3);
        let snap = make_snapshot("w1", true, "some output\nmore stuff\nuser@host:~$");
        assert!(detect_silent_stall(&state, &snap).is_some());
    }

    #[test]
    fn silent_stall_not_at_prompt() {
        let state = make_state("w1", 3);
        let snap = make_snapshot("w1", true, "compiling agenticlaw...\nRunning tests");
        assert!(detect_silent_stall(&state, &snap).is_none());
    }

    #[test]
    fn rabbit_holing_detected_after_threshold() {
        let mut state = make_state("w1", 0);
        state.cycles_off_frontier = 3;
        let snap = make_snapshot("w1", true, "working on something");
        assert!(detect_rabbit_holing(&state, &snap).is_some());
    }

    #[test]
    fn rabbit_holing_not_detected_below_threshold() {
        let mut state = make_state("w1", 0);
        state.cycles_off_frontier = 2;
        let snap = make_snapshot("w1", true, "working on something");
        assert!(detect_rabbit_holing(&state, &snap).is_none());
    }

    #[test]
    fn silent_stall_not_when_recently_active() {
        let state = make_state("w1", 1); // only 1 cycle unchanged
        let snap = make_snapshot("w1", true, "user@host:~$");
        assert!(detect_silent_stall(&state, &snap).is_none());
    }
}
