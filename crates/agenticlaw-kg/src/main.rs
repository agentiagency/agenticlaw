//! agenticlaw-kg CLI — run KG executor against GitHub issues.
//!
//! Now uses the node type registry for recursive descent.
//! The executor decomposes issue → analysis/impl/pr → leaf nodes,
//! each with CODE-prepared FEAR/EGO/PURPOSE.

use agenticlaw_agent::runtime::{AgentConfig, AgentRuntime};
use agenticlaw_kg::{Executor, LocalFsDriver, ResourceDriver, RunConfig};
use agenticlaw_tools::create_default_registry;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "agenticlaw-kg", version = env!("CARGO_PKG_VERSION"), about = "Knowledge Graph Executor — recursive agent tree for issue/PR workflows")]
struct Cli {
    /// Issue number to process
    #[arg(short, long)]
    issue: u64,

    /// One-sentence purpose
    #[arg(short, long)]
    purpose: String,

    /// Target repository (e.g. agentiagency/agentimolt-v03)
    #[arg(short, long, default_value = "agentiagency/agentimolt-v03")]
    repo: String,

    /// Workspace root for agent tools
    #[arg(short, long, default_value = ".")]
    workspace: PathBuf,

    /// Run output directory
    #[arg(long, default_value_t = default_runs_dir())]
    runs_dir: String,

    /// System prompt file
    #[arg(long)]
    system_prompt: Option<PathBuf>,

    /// Print the node tree and exit (dry run)
    #[arg(long)]
    dry_run: bool,
}

fn default_runs_dir() -> String {
    dirs::home_dir()
        .map(|h: std::path::PathBuf| h.join("tmp/kg-runs").display().to_string())
        .unwrap_or_else(|| "/tmp/kg-runs".into())
}

fn print_tree(reg: &agenticlaw_kg::NodeTypeRegistry, id: &str, depth: usize) {
    let Some(nt) = reg.get(id) else { return };
    let indent = "  ".repeat(depth);
    let kind = if nt.is_leaf { "[L]" } else { "[P]" };
    let role = &nt.role;
    let taxonomy = nt.taxonomy_ref.as_deref().unwrap_or("-");
    println!(
        "{}{} {} ({}) role={} taxonomy={}",
        indent, kind, nt.id, nt.name, role, taxonomy
    );
    if let Some(criterion) = &nt.success_criterion {
        println!("{}    ✓ {}", indent, criterion);
    }
    for child_id in &nt.children {
        print_tree(reg, child_id, depth + 1);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agenticlaw_kg=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Dry run: print the node tree
    if cli.dry_run {
        let reg = agenticlaw_kg::default_issue_registry();
        println!("=== KG Node Tree for issue #{} ===\n", cli.issue);
        print_tree(&reg, "issue", 0);
        println!("\nTotal nodes: {}", reg.all_ids().len());
        return Ok(());
    }

    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    let workspace = cli.workspace.canonicalize().unwrap_or(cli.workspace);
    let tools = create_default_registry(&workspace);
    let config = AgentConfig {
        workspace_root: workspace.clone(),
        ..Default::default()
    };

    let runtime = Arc::new(AgentRuntime::new(&api_key, tools, config));
    let driver = Arc::new(LocalFsDriver::new(&cli.runs_dir));
    let driver_trait: Arc<dyn agenticlaw_kg::ResourceDriver> = driver.clone();
    let executor = Executor::new(runtime, driver_trait);

    let system_prompt = if let Some(path) = cli.system_prompt {
        std::fs::read_to_string(path)?
    } else {
        format!(
            "You are a focused software engineer working on {}. \
            Follow instructions precisely. Report what you did and what files you changed. \
            Be surgical — do exactly what's asked, nothing more.",
            cli.repo,
        )
    };

    let target = format!("github.com/{}/issues/{}", cli.repo, cli.issue);

    let run_config = RunConfig {
        purpose: cli.purpose,
        target,
        issue: cli.issue,
        system_prompt,
        analysis_prompt: String::new(), // No longer used — registry provides prompts
        context_summary: format!("Workspace: {}", workspace.display()),
        max_iterations: 3,
    };

    let manifest = executor.run_issue(run_config).await?;

    println!("\n=== Run Complete ===");
    println!("Run ID: {}", manifest.run_id);
    println!("Outcome: {}", manifest.outcome);
    println!("Tokens: {}", manifest.total_tokens);
    println!("Wall: {}ms", manifest.total_wall_ms);
    println!(
        "Location: {}",
        ResourceDriver::run_location(driver.as_ref(), &manifest.run_id)
    );

    // Print node tree with results
    println!("\nNode Results:");
    for (addr, node) in &manifest.nodes {
        let icon = match node.status {
            agenticlaw_kg::NodeState::Success => "✓",
            agenticlaw_kg::NodeState::Failed => "✗",
            _ => "○",
        };
        println!(
            "  {} {} ({} tokens, {}ms)",
            icon, addr, node.tokens, node.wall_ms
        );
    }

    Ok(())
}
