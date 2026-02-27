//! Agenticlaw Gateway — web portal + terminal chat

use agenticlaw_core::{AuthConfig, AuthMode, BindMode, GatewayConfig};
use agenticlaw_gateway::{start_gateway, ExtendedConfig};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(
    name = "agenticlaw-gateway",
    about = "Agenticlaw AI Agent — gateway and chat"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the web gateway server
    Gateway {
        #[arg(short, long, default_value = "18789")]
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
        }) => {
            agenticlaw_gateway::tui::run_tui(workspace, session, model, resume).await?;
        }

        Some(Commands::Version) => {
            println!("agenticlaw v{}", env!("CARGO_PKG_VERSION"));
        }

        // No subcommand = chat
        None => {
            agenticlaw_gateway::tui::run_tui(None, None, None, false).await?;
        }
    }

    Ok(())
}
