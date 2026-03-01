//! agenticlaw — conscious AI agent runtime
//!
//! `agenticlaw` is a bee. It reads ~/.openclaw/openclaw.json for identity.
//!
//! Usage:
//!   agenticlaw                             → start (gateway + consciousness)
//!   agenticlaw --no-consciousness          → gateway only (lightweight/customer)
//!   agenticlaw chat --session X            → TUI chat (connects to service or embedded)
//!   agenticlaw status                      → health check
//!   agenticlaw install                     → install systemd service
//!   agenticlaw version                     → show version

use agenticlaw_consciousness::config::ConsciousnessConfig;
use agenticlaw_consciousness::stack::ConsciousnessStack;
use agenticlaw_core::openclaw_config;
use agenticlaw_core::{AuthConfig, AuthMode, BindMode, GatewayConfig, OpenclawConfig};
use agenticlaw_gateway::{start_gateway, ExtendedConfig};
use clap::{Parser, Subcommand};
use std::net::TcpStream;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_PORT: u16 = 18789;

#[derive(Parser)]
#[command(
    name = "agenticlaw",
    about = "Conscious AI agent runtime — a bee",
    version = env!("CARGO_PKG_VERSION"),
    long_about = "agenticlaw is a conscious AI agent runtime.\n\
                   Default: starts gateway + consciousness stack.\n\
                   Reads ~/.openclaw/openclaw.json for identity and config.\n\
                   Loads AGENTS.md, SOUL.md, IDENTITY.md, USER.md, TOOLS.md,\n\
                   HEARTBEAT.md, MEMORY.md from workspace into session context.\n\
                   Use --no-consciousness for lightweight gateway-only mode."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Session name (triggers TUI chat mode when provided without subcommand)
    #[arg(short, long, global = true)]
    session: Option<String>,

    /// Workspace directory (default: from openclaw.json or ~/.openclaw)
    #[arg(short, long, global = true)]
    workspace: Option<PathBuf>,

    /// Port for the gateway server
    #[arg(short, long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Bind mode: lan or loopback
    #[arg(short, long, default_value = "lan")]
    bind: String,

    /// Auth token (or set AGENTICLAW_GATEWAY_TOKEN)
    #[arg(short, long)]
    token: Option<String>,

    /// Disable consciousness stack (lightweight gateway-only mode)
    #[arg(long, default_value_t = false)]
    no_consciousness: bool,

    /// Disable authentication
    #[arg(long, default_value_t = false)]
    no_auth: bool,

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
    /// Start the gateway daemon (used by systemd service)
    Gateway {
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(short, long, default_value = "lan")]
        bind: String,
        #[arg(short, long)]
        token: Option<String>,
        #[arg(long)]
        no_auth: bool,
        #[arg(short, long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        system_prompt: Option<String>,
    },
    /// Chat with the agent (connects to running service, falls back to embedded)
    Chat {
        #[arg(short, long)]
        workspace: Option<PathBuf>,
        #[arg(short, long)]
        session: Option<String>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        resume: bool,
        /// Force embedded mode (don't try to connect to service)
        #[arg(long)]
        embedded: bool,
        /// Gateway port to connect to
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// Check health of running gateway
    Status {
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// Install systemd user service
    Install {
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// Remove systemd user service
    Uninstall,
    /// Restart the systemd service
    Restart,
    /// Show version
    Version,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let oc = OpenclawConfig::discover();

    match cli.command {
        Some(Commands::Gateway {
            port,
            bind,
            token,
            no_auth,
            workspace,
            system_prompt,
        }) => {
            init_tracing();

            let workspace_root = workspace.unwrap_or_else(|| resolve_home(None, &oc));

            // Load bootstrap identity files
            let bootstrap = openclaw_config::load_bootstrap_files(&workspace_root);
            let bootstrap_prompt = openclaw_config::bootstrap_to_system_prompt(&bootstrap);
            let merged_prompt = match (bootstrap_prompt, system_prompt) {
                (Some(bp), Some(sp)) => Some(format!("{}\n\n{}", bp, sp)),
                (Some(bp), None) => Some(bp),
                (None, sp) => sp,
            };

            let bind_mode = match bind.as_str() {
                "loopback" | "localhost" | "127.0.0.1" => BindMode::Loopback,
                _ => BindMode::Lan,
            };
            let auth = if no_auth {
                AuthConfig {
                    mode: AuthMode::None,
                    token: None,
                }
            } else {
                AuthConfig {
                    mode: AuthMode::Token,
                    token,
                }
            };

            let config = ExtendedConfig {
                gateway: GatewayConfig {
                    port,
                    bind: bind_mode,
                    auth,
                },
                anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
                workspace_root,
                system_prompt: merged_prompt,
            };
            start_gateway(config).await?;
        }

        Some(Commands::Chat {
            workspace,
            session,
            model,
            resume,
            embedded,
            port,
        }) => {
            let workspace = workspace.or_else(|| Some(resolve_home(None, &oc)));
            let model = model.or_else(|| oc.default_model());
            let session_name =
                session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());

            if !embedded {
                if let Ok(health) = agenticlaw_gateway::service::check_health(port).await {
                    let version = health["version"].as_str().unwrap_or("?");
                    eprintln!(
                        "Connecting to agenticlaw gateway v{} on port {}...",
                        version, port
                    );

                    let token = std::env::var("RUSTCLAW_GATEWAY_TOKEN")
                        .or_else(|_| std::env::var("OPENCLAW_GATEWAY_TOKEN"))
                        .ok();

                    return agenticlaw_gateway::tui_client::run_tui_client(
                        port,
                        session_name,
                        token,
                    )
                    .await;
                }
                eprintln!(
                    "No running gateway on port {}. Falling back to embedded mode.",
                    port
                );
                eprintln!("(Start the service with: agenticlaw install)\n");
            }

            agenticlaw_gateway::tui::run_tui(workspace, Some(session_name), model, resume).await?;
        }

        Some(Commands::Status { port }) => {
            match agenticlaw_gateway::service::check_health(port).await {
                Ok(health) => {
                    let version = health["version"].as_str().unwrap_or("?");
                    let sessions = health["sessions"].as_u64().unwrap_or(0);
                    let tools = health["tools"].as_u64().unwrap_or(0);
                    let layer = health["layer"].as_str();

                    println!("agenticlaw v{} running on port {}", version, port);
                    println!("  Sessions: {}", sessions);
                    println!("  Tools:    {}", tools);
                    if let Some(l) = layer {
                        println!("  Layer:    {}", l);
                    }

                    if let Ok(resp) =
                        reqwest::get(format!("http://127.0.0.1:{}/surface", port)).await
                    {
                        if let Ok(surface) = resp.json::<serde_json::Value>().await {
                            let provides =
                                surface["provides"].as_array().map(|a| a.len()).unwrap_or(0);
                            println!("  Capabilities: {}", provides);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Gateway not reachable on port {}", port);
                    eprintln!("  Error: {}", e);
                    eprintln!("  Start with: agenticlaw install");
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Install { port }) => agenticlaw_gateway::service::install(port)?,
        Some(Commands::Uninstall) => agenticlaw_gateway::service::uninstall()?,
        Some(Commands::Restart) => agenticlaw_gateway::service::restart()?,

        Some(Commands::Version) => {
            println!("agenticlaw v{}", env!("CARGO_PKG_VERSION"));
        }

        // No subcommand — connect to running service, or start fresh
        None => {
            let session_name = cli
                .session
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());

            // Try connecting to running service first
            if let Ok(health) = agenticlaw_gateway::service::check_health(cli.port).await {
                let version = health["version"].as_str().unwrap_or("?");
                eprintln!(
                    "Connecting to agenticlaw gateway v{} on port {}...",
                    version, cli.port
                );

                let token = std::env::var("RUSTCLAW_GATEWAY_TOKEN")
                    .or_else(|_| std::env::var("OPENCLAW_GATEWAY_TOKEN"))
                    .ok();

                agenticlaw_gateway::tui_client::run_tui_client(cli.port, session_name, token)
                    .await?;
            } else if cli.no_consciousness {
                init_tracing();
                start_gateway_only(&cli, &oc).await?;
            } else {
                // Nothing running — start consciousness
                init_tracing();
                start_conscious(&cli, &oc).await?;
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

async fn start_conscious(cli: &Cli, oc: &OpenclawConfig) -> anyhow::Result<()> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let home = resolve_home(cli.workspace.as_ref(), oc);
    let workspace = home.join("consciousness");
    std::fs::create_dir_all(&workspace)?;

    let souls = cli
        .souls
        .as_ref()
        .map(|s| expand_tilde(s))
        .unwrap_or_else(|| {
            let candidates = [
                PathBuf::from("consciousness/souls"),
                {
                    let h = std::env::var("HOME").unwrap_or_else(|_| "/home/devkit".to_string());
                    PathBuf::from(h).join("agentiagency/agenticlaw/consciousness/souls")
                },
                PathBuf::from("/etc/agenticlaw/souls"),
            ];
            for c in &candidates {
                if c.is_dir() {
                    return c.clone();
                }
            }
            candidates.last().unwrap().clone()
        });

    let config_path = cli
        .config
        .as_ref()
        .map(|p| expand_tilde(p))
        .unwrap_or_else(|| workspace.join("consciousness.toml"));
    let config = ConsciousnessConfig::load(&config_path);

    let port = resolve_port(cli.port);

    println!("╔══════════════════════════════════════════════════╗");
    println!(
        "║         AGENTICLAW v{}                       ║",
        env!("CARGO_PKG_VERSION")
    );
    println!("║              Conscious Agent Runtime             ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  L0  Gateway      :{}  ← you are here        ║", port);
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
    tracing::info!("Home: {}", home.display());

    let stack = ConsciousnessStack::new(workspace, souls, api_key, config);
    stack.launch(cli.birth).await?;

    Ok(())
}

async fn start_gateway_only(cli: &Cli, oc: &OpenclawConfig) -> anyhow::Result<()> {
    let workspace_root = resolve_home(cli.workspace.as_ref(), oc);

    // Load bootstrap identity files
    let bootstrap = openclaw_config::load_bootstrap_files(&workspace_root);
    let bootstrap_prompt = openclaw_config::bootstrap_to_system_prompt(&bootstrap);
    let merged_prompt = match (bootstrap_prompt, cli.system_prompt.clone()) {
        (Some(bp), Some(sp)) => Some(format!("{}\n\n{}", bp, sp)),
        (Some(bp), None) => Some(bp),
        (None, sp) => sp,
    };

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

    let config = ExtendedConfig {
        gateway: GatewayConfig {
            port: resolve_port(cli.port),
            bind: bind_mode,
            auth,
        },
        anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
        workspace_root,
        system_prompt: merged_prompt,
    };
    start_gateway(config).await?;
    Ok(())
}

/// Check if a TCP port is in use on localhost.
fn port_in_use(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// Resolve the agenticlaw home directory:
/// 1. Explicit --workspace flag or env var
/// 2. openclaw.json agents.defaults.workspace
/// 3. Port 18789 free → ~/.openclaw (drop-in for openclaw)
/// 4. Port 18789 occupied → ~/.agenticlaw (coexist)
fn resolve_home(explicit: Option<&PathBuf>, oc: &OpenclawConfig) -> PathBuf {
    if let Some(p) = explicit {
        return p.clone();
    }
    if let Ok(env_ws) = std::env::var("AGENTICLAW_WORKSPACE") {
        return PathBuf::from(env_ws);
    }
    if let Ok(env_ws) = std::env::var("RUSTCLAW_WORKSPACE") {
        return PathBuf::from(env_ws);
    }

    // openclaw.json workspace (strip /workspace suffix to get home)
    let oc_ws = oc.workspace();
    if oc_ws.exists() {
        return oc_ws.parent().map(PathBuf::from).unwrap_or(oc_ws);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let home = PathBuf::from(home);

    if port_in_use(18789) {
        home.join(".agenticlaw")
    } else {
        home.join(".openclaw")
    }
}

/// Resolve the default port: if requested port is occupied, scan for a free one.
fn resolve_port(requested: u16) -> u16 {
    if !port_in_use(requested) {
        return requested;
    }
    for p in [18799u16] {
        if !port_in_use(p) {
            return p;
        }
    }
    for p in 18800..=18899 {
        if !port_in_use(p) {
            return p;
        }
    }
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
