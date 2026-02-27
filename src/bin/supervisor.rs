use agenticlaw::supervisor::poll::run_supervisor;
use agenticlaw::supervisor::types::{BackoffConfig, SupervisorConfig};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "agenticlaw-supervisor",
    about = "Supervisor/Guru polling loop for tmux worker sessions"
)]
struct Cli {
    /// Base directory for molts workspace (e.g. ~/agentiagency/molts/ws2-7)
    #[arg(long, default_value = ".")]
    molts_base: String,

    /// Base polling interval in milliseconds
    #[arg(long, default_value_t = 8000)]
    poll_base_ms: u64,

    /// Maximum polling interval in milliseconds
    #[arg(long, default_value_t = 60000)]
    poll_max_ms: u64,

    /// Seconds of no change before declaring frozen
    #[arg(long, default_value_t = 240)]
    frozen_threshold_secs: u64,

    /// Log detections but don't send interventions
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Emit JSON status on stdout each poll cycle (for piping to conductor)
    #[arg(long, default_value_t = false)]
    json_stdout: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let config = SupervisorConfig {
        molts_base: cli.molts_base,
        backoff: BackoffConfig {
            base_ms: cli.poll_base_ms,
            multiplier: 1.5,
            max_ms: cli.poll_max_ms,
        },
        frozen_threshold_secs: cli.frozen_threshold_secs,
        dry_run: cli.dry_run,
        json_stdout: cli.json_stdout,
    };

    run_supervisor(config).await;
}
