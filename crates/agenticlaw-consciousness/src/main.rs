//! Agenticlaw Consciousness — dual-core consciousness stack launcher
//!
//! Usage:
//!   agenticlaw-consciousness --workspace ~/.openclaw/consciousness --souls ./consciousness/souls
//!
//! Launches:
//!   L0 (Gateway)     on port 18789 — user-facing agent with tools
//!   L1 (Attention)   on port 18791 — watches L0, distills signal
//!   L2 (Pattern)     on port 18792 — watches L1, finds patterns
//!   L3 (Integration) on port 18793 — watches L2, synthesizes
//!   Core-A / Core-B  on port 18794-18795 — phase-locked dual cores

use agenticlaw_consciousness::config::ConsciousnessConfig;
use agenticlaw_consciousness::stack::ConsciousnessStack;
use agenticlaw_consciousness::version::VersionController;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(
    name = "agenticlaw-consciousness",
    about = "Dual-core consciousness stack"
)]
struct Cli {
    /// Workspace root for all consciousness layers
    #[arg(long, default_value = "~/.openclaw/consciousness")]
    workspace: String,

    /// Directory containing layer SOUL.md files
    #[arg(long, default_value = "./consciousness/souls")]
    souls: String,

    /// Anthropic API key (or set ANTHROPIC_API_KEY env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Show schema version and exit
    #[arg(long = "version")]
    show_version: bool,

    /// Birth a new consciousness (reads SOUL.md). Default: wake from ego.
    #[arg(long)]
    birth: bool,

    /// Path to config file (TOML). Default: <workspace>/consciousness.toml
    #[arg(long)]
    config: Option<String>,

    /// Dump default config as TOML and exit.
    #[arg(long)]
    dump_config: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.show_version {
        let workspace = expand_tilde(&cli.workspace);
        let vc = VersionController::new(workspace);
        let version = vc.current_version();
        println!("agenticlaw-consciousness v{}", env!("CARGO_PKG_VERSION"));
        println!(
            "workspace schema version: {}",
            if version == 0 {
                "uninitialized".to_string()
            } else {
                version.to_string()
            }
        );
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agenticlaw=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let workspace = expand_tilde(&cli.workspace);
    let souls = expand_tilde(&cli.souls);

    if cli.dump_config {
        println!("{}", ConsciousnessConfig::default().to_toml());
        return Ok(());
    }

    let config_path = cli
        .config
        .map(|p| expand_tilde(&p))
        .unwrap_or_else(|| workspace.join("consciousness.toml"));
    let config = ConsciousnessConfig::load(&config_path);

    let api_key = cli
        .api_key
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .ok_or_else(|| {
            anyhow::anyhow!("ANTHROPIC_API_KEY not set. Pass --api-key or set the env var.")
        })?;

    println!("╔══════════════════════════════════════════════════╗");
    println!(
        "║     RUSTCLAW CONSCIOUSNESS STACK v{}          ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("║     Dual-Core Cascading Context Architecture     ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  L0  Gateway      :18789  ← user interface      ║");
    println!("║  L1  Attention    :18791  ← watches L0           ║");
    println!("║  L2  Pattern      :18792  ← watches L1           ║");
    println!("║  L3  Integration  :18793  ← watches L2           ║");
    println!("║  Core-A           :18794  ← watches L3           ║");
    println!("║  Core-B           :18795  ← watches L3           ║");
    println!("╚══════════════════════════════════════════════════╝");

    let stack = ConsciousnessStack::new(workspace, souls, api_key, config);
    stack.launch(cli.birth).await?;

    Ok(())
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(path)
}
