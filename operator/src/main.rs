//! Rustclaw Operator — build, test, and push agent container images

mod builder;
mod live_tester;
mod mock_provider;
mod policy;
mod proxy;
mod tester;

use clap::{Parser, Subcommand};
use policy::Role;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "operator", about = "Rustclaw container orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build agent container images
    Build {
        /// Build a specific role only
        #[arg(long)]
        role: Option<String>,
        /// Push to registry after build
        #[arg(long)]
        push: bool,
        /// Registry URL (e.g., 123456.dkr.ecr.us-east-1.amazonaws.com)
        #[arg(long)]
        registry: Option<String>,
    },
    /// Test agent containers against policy
    Test {
        /// Test a specific role
        #[arg(long)]
        role: Option<String>,
        /// Test all roles
        #[arg(long)]
        all: bool,
        /// Custom policy file to test
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Sub-policies to overlay
        #[arg(long)]
        sub_policies: Option<PathBuf>,
        /// Run against live containers (not just policy checks)
        #[arg(long)]
        live: bool,
    },
    /// Push images to registry
    Push {
        /// Registry URL
        #[arg(long)]
        registry: String,
        /// Push a specific role only
        #[arg(long)]
        role: Option<String>,
    },
    /// Show policy details for a role
    Policy {
        /// Role to inspect
        role: String,
    },
    /// List all roles and their capabilities
    Roles,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "operator=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Build { role, push, registry } => {
            let project_root = find_project_root()?;
            let reg = registry.as_deref();

            if let Some(role_str) = role {
                let r: Role = role_str.parse()?;
                builder::build_role(&project_root, r, reg)?;
                if push {
                    if let Some(reg) = reg {
                        builder::push_image(r, reg)?;
                    }
                }
            } else {
                builder::build_all(&project_root, reg)?;
                if push {
                    if let Some(reg) = reg {
                        builder::push_all(reg)?;
                    }
                }
            }
        }

        Commands::Test { role, all, policy, sub_policies, live } => {
            if live {
                let r = if let Some(ref role_str) = role {
                    role_str.parse::<Role>()?
                } else if all {
                    // Run live tests for all roles sequentially
                    let mut total_p = 0;
                    let mut total_f = 0;
                    let mut port = 18800u16;
                    for r in Role::all() {
                        let (p, f) = live_tester::run_live_tests(*r, port, None).await?;
                        total_p += p;
                        total_f += f;
                        port += 1;
                    }
                    info!("=== LIVE TOTAL: {}/{} passed ===", total_p, total_p + total_f);
                    if total_f > 0 { std::process::exit(1); }
                    return Ok(());
                } else {
                    anyhow::bail!("--live requires --role ROLE or --all");
                };
                let host_port = 18800u16;
                let (passed, failed) = live_tester::run_live_tests(r, host_port, None).await?;
                if failed > 0 { std::process::exit(1); }
                let _ = passed;
                return Ok(());
            }

            if let Some(policy_path) = policy {
                // Test custom policy
                let mut pol = policy::Policy::load(&policy_path)?;
                if let Some(sub_path) = sub_policies {
                    let sub = policy::Policy::load(&sub_path)?;
                    pol.merge_sub_policy(&sub);
                }
                let role_enum: Role = pol.role.parse()?;
                let runner = tester::TestRunner::new("http://localhost:0", role_enum, pol);
                let (passed, failed) = runner.run_all().await?;
                if failed > 0 {
                    std::process::exit(1);
                }
            } else if let Some(role_str) = role {
                let r: Role = role_str.parse()?;
                let (_, failed) = tester::test_role_policy(r).await?;
                if failed > 0 {
                    std::process::exit(1);
                }
            } else if all {
                let (_, failed) = tester::test_all_policies().await?;
                if failed > 0 {
                    std::process::exit(1);
                }
            } else {
                info!("Specify --role ROLE, --all, or --policy FILE");
            }
        }

        Commands::Push { registry, role } => {
            if let Some(role_str) = role {
                let r: Role = role_str.parse()?;
                builder::push_image(r, &registry)?;
            } else {
                builder::push_all(&registry)?;
            }
        }

        Commands::Policy { role } => {
            let r: Role = role.parse()?;
            let policy_path = format!("policies/{}.json", r.name());
            let pol = policy::Policy::load(&policy_path)?;
            println!("{}", serde_json::to_string_pretty(&pol)?);
        }

        Commands::Roles => {
            println!("Rustclaw Agent Roles (most restricted → least restricted):");
            println!();
            for role in Role::all() {
                let policy_path = format!("policies/{}.json", role.name());
                if let Ok(pol) = policy::Policy::load(&policy_path) {
                    println!("  {:10} tools: allow={:?} deny={:?}",
                        role.name(),
                        pol.tools.allow,
                        pol.tools.deny,
                    );
                } else {
                    println!("  {:10} (policy file not found)", role.name());
                }
            }
        }
    }

    Ok(())
}

fn find_project_root() -> anyhow::Result<PathBuf> {
    // Walk up from CWD looking for Cargo.toml with [workspace]
    let mut dir = std::env::current_dir()?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = std::fs::read_to_string(&cargo_toml)?;
            if content.contains("[workspace]") {
                return Ok(dir);
            }
        }
        // Also check if we're in the operator dir
        if dir.join("operator").join("Cargo.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            // Default to parent of operator
            return Ok(PathBuf::from(".").join(".."));
        }
    }
}
