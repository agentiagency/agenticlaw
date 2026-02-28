//! Agenticlaw — AI agent runtime bee
//!
//! `agenticlaw` is a bee. The CLI is a frontend to the systemd service.
//! `agenticlaw chat` connects to the running service via WebSocket.
//! `agenticlaw gateway` starts the daemon directly (used by systemd).

use agenticlaw_core::{AuthConfig, AuthMode, BindMode, GatewayConfig};
use agenticlaw_gateway::{start_gateway, ExtendedConfig};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_PORT: u16 = 18789;

#[derive(Parser)]
#[command(name = "agenticlaw", about = "Agenticlaw AI agent runtime — a bee")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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

    match cli.command {
        Some(Commands::Gateway {
            port,
            bind,
            token,
            no_auth,
            workspace,
            system_prompt,
        }) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "agenticlaw=info,tower_http=info".into()),
                )
                .with(tracing_subscriber::fmt::layer())
                .init();

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
            let workspace_root = workspace
                .or_else(|| std::env::var("RUSTCLAW_WORKSPACE").ok().map(PathBuf::from))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            let config = ExtendedConfig {
                gateway: GatewayConfig {
                    port,
                    bind: bind_mode,
                    auth,
                },
                anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
                workspace_root,
                system_prompt,
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
            let session_name =
                session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());

            if !embedded {
                // Try connecting to running service first
                match agenticlaw_gateway::service::check_health(port).await {
                    Ok(health) => {
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
                    Err(_) => {
                        eprintln!(
                            "No running gateway on port {}. Falling back to embedded mode.",
                            port
                        );
                        eprintln!("(Start the service with: agenticlaw install)\n");
                    }
                }
            }

            // Embedded fallback
            agenticlaw_gateway::tui::run_tui(workspace, Some(session_name), model, resume).await?;
        }

        Some(Commands::Status { port }) => {
            match agenticlaw_gateway::service::check_health(port).await {
                Ok(health) => {
                    let version = health["version"].as_str().unwrap_or("?");
                    let sessions = health["sessions"].as_u64().unwrap_or(0);
                    let tools = health["tools"].as_u64().unwrap_or(0);
                    let layer = health["layer"].as_str();

                    println!("✓ agenticlaw v{} running on port {}", version, port);
                    println!("  Sessions: {}", sessions);
                    println!("  Tools:    {}", tools);
                    if let Some(l) = layer {
                        println!("  Layer:    {}", l);
                    }

                    // Also try sacred endpoints
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
                    eprintln!("✗ Gateway not reachable on port {}", port);
                    eprintln!("  Error: {}", e);
                    eprintln!("  Start with: agenticlaw install");
                    std::process::exit(1);
                }
            }
        }

        Some(Commands::Install { port }) => {
            agenticlaw_gateway::service::install(port)?;
        }

        Some(Commands::Uninstall) => {
            agenticlaw_gateway::service::uninstall()?;
        }

        Some(Commands::Restart) => {
            agenticlaw_gateway::service::restart()?;
        }

        Some(Commands::Version) => {
            println!("agenticlaw v{}", env!("CARGO_PKG_VERSION"));
        }

        // No subcommand = chat (try service first)
        None => {
            let session_name = uuid::Uuid::new_v4().to_string()[..8].to_string();

            match agenticlaw_gateway::service::check_health(DEFAULT_PORT).await {
                Ok(health) => {
                    let version = health["version"].as_str().unwrap_or("?");
                    eprintln!(
                        "Connecting to agenticlaw gateway v{} on port {}...",
                        version, DEFAULT_PORT
                    );

                    let token = std::env::var("RUSTCLAW_GATEWAY_TOKEN")
                        .or_else(|_| std::env::var("OPENCLAW_GATEWAY_TOKEN"))
                        .ok();

                    agenticlaw_gateway::tui_client::run_tui_client(
                        DEFAULT_PORT,
                        session_name,
                        token,
                    )
                    .await?;
                }
                Err(_) => {
                    eprintln!("No running gateway. Starting embedded mode.");
                    eprintln!("(For persistent sessions, run: agenticlaw install)\n");
                    agenticlaw_gateway::tui::run_tui(None, Some(session_name), None, false).await?;
                }
            }
        }
    }

    Ok(())
}
