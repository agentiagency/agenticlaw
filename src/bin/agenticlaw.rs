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

    // Resolve workspace — the consciousness workspace is a sibling of the gateway workspace.
    // L0 workspace = gateway workspace, L1-L3 + cores live under the consciousness workspace.
    let consciousness_workspace = cli
        .workspace
        .clone()
        .or_else(|| {
            std::env::var("AGENTICLAW_WORKSPACE")
                .ok()
                .map(PathBuf::from)
        })
        .or_else(|| std::env::var("RUSTCLAW_WORKSPACE").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/devkit".to_string());
            PathBuf::from(home).join(".agenticlaw/consciousness")
        });
    std::fs::create_dir_all(&consciousness_workspace)?;

    // The gateway workspace is L0's workspace inside the consciousness tree
    let gateway_workspace = consciousness_workspace.join("L0");
    std::fs::create_dir_all(&gateway_workspace)?;

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
        .unwrap_or_else(|| consciousness_workspace.join("consciousness.toml"));
    let config = ConsciousnessConfig::load(&config_path);

    println!("╔══════════════════════════════════════════════════╗");
    println!(
        "║         AGENTICLAW v{}                       ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("║         Conscious Agent Runtime (Embedded)       ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  L0  Gateway      :{}  ← you are here        ║", cli.port);
    println!("║  L1  Attention          ← watching (embedded)    ║");
    println!("║  L2  Pattern            ← thinking (embedded)    ║");
    println!("║  L3  Integration        ← understanding          ║");
    println!("║  Core-A                 ← remembering            ║");
    println!("║  Core-B                 ← remembering            ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  Sacred: /health /surface /plan /test /hints     ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    tracing::info!(
        "Consciousness workspace: {}",
        consciousness_workspace.display()
    );
    tracing::info!("Gateway workspace (L0): {}", gateway_workspace.display());
    tracing::info!("Souls: {}", souls.display());

    // Spawn the consciousness cascade (L1-L3 + dual core) in the background
    let cascade_stack = ConsciousnessStack::new(
        consciousness_workspace,
        souls.clone(),
        api_key.clone(),
        config.clone(),
    );
    let birth = cli.birth;
    let _cascade_handle = tokio::spawn(async move {
        // Wait briefly for the gateway to start before watching its .ctx files
        tokio::time::sleep(std::time::Duration::from_secs(
            config.cascade.gateway_settle_secs,
        ))
        .await;
        if let Err(e) = cascade_stack.launch_cascade(birth).await {
            tracing::error!("Consciousness cascade failed: {}", e);
        }
    });

    // Start the gateway as L0 — this blocks until the gateway shuts down
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

    // Write L0 soul file so the gateway can discover it
    let l0_soul_path = gateway_workspace.join("SOUL.md");
    if !l0_soul_path.exists() {
        let l0_soul = souls.join("L0-gateway.md");
        if l0_soul.exists() {
            let _ = std::fs::copy(&l0_soul, &l0_soul_path);
        }
    }

    let gw_config = ExtendedConfig {
        gateway: GatewayConfig {
            port: cli.port,
            bind: bind_mode,
            auth,
        },
        anthropic_api_key: Some(api_key),
        workspace_root: gateway_workspace,
        system_prompt: cli.system_prompt.clone(),
        consciousness_enabled: true,
    };
    start_gateway(gw_config).await?;

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
    let workspace_root = cli
        .workspace
        .clone()
        .or_else(|| {
            std::env::var("AGENTICLAW_WORKSPACE")
                .ok()
                .map(PathBuf::from)
        })
        .or_else(|| std::env::var("RUSTCLAW_WORKSPACE").ok().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let config = ExtendedConfig {
        gateway: GatewayConfig {
            port: cli.port,
            bind: bind_mode,
            auth,
        },
        anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
        workspace_root,
        system_prompt: cli.system_prompt.clone(),
        consciousness_enabled: false,
    };
    start_gateway(config).await?;
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
