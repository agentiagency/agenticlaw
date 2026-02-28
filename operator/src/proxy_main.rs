//! protectgateway binary — policy-enforcing reverse proxy for agenticlaw

mod mock_provider;
mod policy;
mod proxy;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Parser)]
#[command(name = "protectgateway", about = "Policy-enforcing proxy for agenticlaw")]
struct Cli {
    /// Listen address
    #[arg(long, default_value = "0.0.0.0:18789")]
    listen: String,

    /// Upstream agenticlaw address
    #[arg(long, default_value = "127.0.0.1:18790")]
    upstream: String,

    /// Path to policy JSON file
    #[arg(long, default_value = "/etc/agenticlaw/policy.json")]
    policy: PathBuf,

    /// Directory for sub-policy overlays
    #[arg(long)]
    sub_policies: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "protectgateway=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Load base policy
    let mut pol = policy::Policy::load(&cli.policy)?;
    info!("Loaded policy: role={}", pol.role);

    // Merge sub-policies if directory provided
    if let Some(sub_dir) = &cli.sub_policies {
        if sub_dir.is_dir() {
            for entry in std::fs::read_dir(sub_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    match policy::Policy::load(&path) {
                        Ok(sub) => {
                            info!("Merging sub-policy: {}", path.display());
                            pol.merge_sub_policy(&sub);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load sub-policy {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
    }

    let state = Arc::new(proxy::ProxyState {
        policy: pol,
        upstream_url: format!("http://{}", cli.upstream),
        upstream_ws_url: format!("ws://{}", cli.upstream),
    });

    let router = proxy::create_router(state);
    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    info!("protectgateway listening on {}", cli.listen);
    axum::serve(listener, router).await?;

    Ok(())
}
