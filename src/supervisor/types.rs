use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub name: String,
    pub exists: bool,
    pub pane_content: String,
    pub pane_hash: u64,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Active,
    Idle,
    Frozen,
    RabbitHoling,
    Deranged,
    InfiniteLoop,
    Dead,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Idle => write!(f, "idle"),
            Self::Frozen => write!(f, "frozen"),
            Self::RabbitHoling => write!(f, "rabbit-holing"),
            Self::Deranged => write!(f, "deranged"),
            Self::InfiniteLoop => write!(f, "infinite-loop"),
            Self::Dead => write!(f, "dead"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub name: String,
    pub status: SessionStatus,
    pub card: Option<String>,
    pub context_pct: Option<u8>,
    pub frontier_summary: Option<String>,
    pub consecutive_unchanged: u32,
    pub recent_commands: Vec<String>,
    pub retry_ops: HashMap<String, u32>,
    pub last_snapshot_hash: Option<u64>,
    pub last_active_at: DateTime<Utc>,
    pub prev_frontier_summary: Option<String>,
    pub cycles_off_frontier: u32,
}

impl SessionState {
    pub fn new(name: String) -> Self {
        Self {
            name,
            status: SessionStatus::Active,
            card: None,
            context_pct: None,
            frontier_summary: None,
            consecutive_unchanged: 0,
            recent_commands: Vec::new(),
            retry_ops: HashMap::new(),
            last_snapshot_hash: None,
            last_active_at: Utc::now(),
            prev_frontier_summary: None,
            cycles_off_frontier: 0,
        }
    }

    pub fn update_from_snapshot(&mut self, snapshot: &SessionSnapshot) {
        if let Some(prev_hash) = self.last_snapshot_hash {
            if prev_hash == snapshot.pane_hash {
                self.consecutive_unchanged += 1;
            } else {
                self.consecutive_unchanged = 0;
                self.last_active_at = Utc::now();
            }
        }
        self.last_snapshot_hash = Some(snapshot.pane_hash);
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorState {
    pub sessions: HashMap<String, SessionState>,
    pub card_map: HashMap<String, String>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub current_backoff_ms: u64,
}

impl SupervisorState {
    pub fn new(backoff_base_ms: u64) -> Self {
        Self {
            sessions: HashMap::new(),
            card_map: HashMap::new(),
            last_poll_at: None,
            current_backoff_ms: backoff_base_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackoffConfig {
    pub base_ms: u64,
    pub multiplier: f64,
    pub max_ms: u64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base_ms: 8000,
            multiplier: 1.5,
            max_ms: 60000,
        }
    }
}

#[derive(Debug, Clone)]
pub enum FailureMode {
    Frozen {
        session: String,
        unchanged_secs: u64,
    },
    Deranged {
        session: String,
        repeated_pattern: String,
        count: u32,
    },
    RabbitHoling {
        session: String,
        cycles_off_frontier: u32,
    },
    InfiniteLoop {
        session: String,
        operation: String,
        retries: u32,
    },
    SilentStall {
        session: String,
    },
    Dead {
        session: String,
    },
}

impl FailureMode {
    pub fn session_name(&self) -> &str {
        match self {
            Self::Frozen { session, .. }
            | Self::Deranged { session, .. }
            | Self::RabbitHoling { session, .. }
            | Self::InfiniteLoop { session, .. }
            | Self::SilentStall { session }
            | Self::Dead { session } => session,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ConductorCommand {
    SpawnWorker {
        name: String,
        card: Option<String>,
        briefing: Option<String>,
    },
    KillWorker {
        name: String,
    },
    StatusReport,
    SendToWorker {
        name: String,
        keys: String,
    },
    ReassignCard {
        name: String,
        card: String,
        briefing: Option<String>,
    },
    RotateWorker {
        name: String,
        briefing: Option<String>,
    },
    ContextReset {
        name: String,
        briefing: Option<String>,
    },
    ListWorkers,
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub molts_base: String,
    pub backoff: BackoffConfig,
    pub frozen_threshold_secs: u64,
    pub dry_run: bool,
    pub json_stdout: bool,
}
