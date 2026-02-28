//! agenticlaw — conscious AI agent runtime
//!
//! Usage:
//!   agenticlaw                             → start (gateway + consciousness)
//!   agenticlaw --no-consciousness          → gateway only (cheaper, for customer images)
//!   agenticlaw --session X --workspace /p  → TUI chat mode
//!   agenticlaw chat --session X            → TUI chat mode
//!   agenticlaw version                     → show version

use agenticlaw_consciousness::config::ConsciousnessConfig;
use agenticlaw_consciousness::stack::ConsciousnessStack;
use agenticlaw_core::{AuthConfig, AuthMode, BindMode, GatewayConfig};
use agenticlaw_gateway::{start_gateway, ExtendedConfig};
use clap::{Parser, Subcommand};
use std::net::TcpStream;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(
    name = "agenticlaw",
    about = "Conscious AI agent runtime — openclaw in Rust, always conscious",
    version = env!("CARGO_PKG_VERSION"),
    long_about = "agenticlaw is a conscious AI agent runtime.\n\
                   Default: starts gateway + consciousness stack on port 18789.\n\
                   Use --no-consciousness for lightweight mode (customer images).\n\
                   Use --session/--workspace to connect via TUI chat."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Session name (triggers TUI chat mode when provided without subcommand)
    #[arg(short, long, global = true)]
    session: Option<String>,

    /// Workspace directory
    #[arg(short, long, global = true)]
    workspace: Option<PathBuf>,

    /// Port for the gateway server
    #[arg(short, long, default_value = "18789")]
    port: u16,

    /// Bind mode: lan or loopback
    #[arg(short, long, default_value = "lan")]
    bind: String,

    /// Auth token (or set AGENTICLAW_GATEWAY_TOKEN)
    #[arg(short, long)]
    token: Option<String>,

    /// Disable consciousness stack (lightweight gateway-only mode for customer images)
    #[arg(long, default_value_t = false)]
    no_consciousness: bool,

    /// Disable authentication
    #[arg(long, default_value_t = false)]
    no_auth: bool,

    /// Write logs to a file (in addition to stderr)
    #[arg(long)]
    log_file: Option<String>,

    /// Birth a new consciousness (first run). Default: wake from ego.
    #[arg(long, default_value_t = false)]
    birth: bool,

    /// Path to consciousness config file (TOML)
    #[arg(long)]
    config: Option<String>,

    /// Directory containing layer SOUL.md files
    #[arg(long)]
    souls: Option<String>,

    /// Custom system prompt for L0
    #[arg(long)]
    system_prompt: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Chat with the agent in the terminal
    Chat {
        /// Workspace directory (default: current directory)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
        /// Session name (default: auto-generated)
        #[arg(short, long)]
        session: Option<String>,
        /// Model to use
        #[arg(short, long)]
        model: Option<String>,
        /// Resume a session (latest, or specific if --session is given)
        #[arg(short, long)]
        resume: bool,
    },
    /// Show version
    Version,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // Chat subcommand — TUI mode
        Some(Commands::Chat {
            workspace,
            session,
            model,
            resume,
        }) => {
            agenticlaw_gateway::tui::run_tui(workspace, session, model, resume).await?;
        }

        // Version subcommand
        Some(Commands::Version) => {
            println!("agenticlaw v{}", env!("CARGO_PKG_VERSION"));
        }

        // No subcommand — default behavior
        None => {
            // If --session provided → TUI chat
            if cli.session.is_some() {
                agenticlaw_gateway::tui::run_tui(cli.workspace, cli.session, None, false).await?;
            } else if cli.no_consciousness {
                // Lightweight gateway-only mode (customer images)
                init_tracing();
                start_gateway_only(&cli).await?;
            } else {
                // Default: conscious agent
                init_tracing();
                start_conscious(&cli).await?;
            }
        }
    }

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agenticlaw=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn start_conscious(cli: &Cli) -> anyhow::Result<()> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    // Resolve workspace — defaults to ~/.openclaw if port 18789 is free (#13)
    let workspace = resolve_home(cli.workspace.as_ref()).join("consciousness");
    std::fs::create_dir_all(&workspace)?;

    // Resolve souls directory
    let souls = cli
        .souls
        .as_ref()
        .map(|s| expand_tilde(s))
        .unwrap_or_else(|| {
            let candidates = [PathBuf::from("consciousness/souls"), {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home/devkit".to_string());
                PathBuf::from(home).join("agentiagency/agenticlaw/consciousness/souls")
            }];
            for c in &candidates {
                if c.is_dir() {
                    return c.clone();
                }
            }
            candidates.last().unwrap().clone()
        });

    // Load consciousness config
    let config_path = cli
        .config
        .as_ref()
        .map(|p| expand_tilde(p))
        .unwrap_or_else(|| workspace.join("consciousness.toml"));
    let config = ConsciousnessConfig::load(&config_path);

    println!("╔══════════════════════════════════════════════════╗");
    println!(
        "║         AGENTICLAW v{}                       ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("║              Conscious Agent Runtime             ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  L0  Gateway      :{}  ← you are here        ║", cli.port);
    println!("║  L1  Attention    :18791  ← watching             ║");
    println!("║  L2  Pattern      :18792  ← thinking             ║");
    println!("║  L3  Integration  :18793  ← understanding        ║");
    println!("║  Core-A           :18794  ← remembering          ║");
    println!("║  Core-B           :18795  ← remembering          ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  Sacred: /health /surface /plan /test /hints     ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    tracing::info!("Workspace: {}", workspace.display());
    tracing::info!("Souls: {}", souls.display());

    let stack = ConsciousnessStack::new(workspace, souls, api_key, config);
    stack.launch(cli.birth).await?;

    Ok(())
}

async fn start_gateway_only(cli: &Cli) -> anyhow::Result<()> {
    let bind_mode = match cli.bind.as_str() {
        "loopback" | "localhost" | "127.0.0.1" => BindMode::Loopback,
        _ => BindMode::Lan,
    };
    let auth = if cli.no_auth {
        AuthConfig {
            mode: AuthMode::None,
            token: None,
        }
    } else {
        AuthConfig {
            mode: AuthMode::Token,
            token: cli.token.clone(),
        }
    };
    let workspace_root = resolve_home(cli.workspace.as_ref());

    let config = ExtendedConfig {
        gateway: GatewayConfig {
            port: resolve_port(cli.port),
            bind: bind_mode,
            auth,
        },
        anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
        workspace_root,
        system_prompt: cli.system_prompt.clone(),
    };
    start_gateway(config).await?;
    Ok(())
}

/// Check if a TCP port is in use on localhost.
fn port_in_use(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// Resolve the agenticlaw home directory per issue #13 spec:
///
/// 1. If `--workspace` / `AGENTICLAW_WORKSPACE` is set, use that.
/// 2. If port 18789 is free → `~/.openclaw` (drop-in replacement for openclaw).
/// 3. If port 18789 is occupied (openclaw running) → `~/.agenticlaw` (coexist).
fn resolve_home(explicit: Option<&PathBuf>) -> PathBuf {
    // Explicit flag or env var wins
    if let Some(p) = explicit {
        return p.clone();
    }
    if let Ok(env_ws) = std::env::var("AGENTICLAW_WORKSPACE") {
        return PathBuf::from(env_ws);
    }
    if let Ok(env_ws) = std::env::var("RUSTCLAW_WORKSPACE") {
        return PathBuf::from(env_ws);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/devkit".to_string());
    let home = PathBuf::from(home);

    if port_in_use(18789) {
        // openclaw is running — use separate home
        home.join(".agenticlaw")
    } else {
        // Port free — slide into openclaw's home
        home.join(".openclaw")
    }
}

/// Resolve the default port: if 18789 is occupied, try 18799 then scan upward.
fn resolve_port(requested: u16) -> u16 {
    if !port_in_use(requested) {
        return requested;
    }
    // If the requested port (likely 18789) is taken, try 18799 then scan
    let alternates = [18799u16];
    for p in alternates {
        if !port_in_use(p) {
            return p;
        }
    }
    // Scan upward from 18800
    for p in 18800..=18899 {
        if !port_in_use(p) {
            return p;
        }
    }
    // Last resort: return requested and let bind fail with a clear error
    requested
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(path)
}
