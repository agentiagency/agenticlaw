//! Agenticlaw Tools — modular tool implementations
//!
//! Each tool is a self-contained file in src/tools/.
//! To add a tool: create the file, implement Tool trait, register below.
//! To remove a tool: delete the file, remove from mod.rs and registry below.

pub mod registry;
pub mod tools;

pub use registry::{Tool, ToolRegistry, ToolResult};
pub use tools::spawn::{SpawnTool, SpawnableRuntime, RuntimeHandle};

use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Create the default tool registry with all builtin tools.
///
/// Edit this function to add or remove tools from the agent.
/// Create a runtime handle for the spawn tool. Call this before creating the registry,
/// then set the runtime after constructing AgentRuntime.
pub fn create_runtime_handle() -> RuntimeHandle {
    Arc::new(RwLock::new(None))
}

pub fn create_default_registry(workspace_root: impl AsRef<Path>) -> ToolRegistry {
    create_default_registry_with_spawn(workspace_root, create_runtime_handle())
}

/// Create registry with a shared runtime handle for the spawn tool.
/// After constructing AgentRuntime, call `runtime_handle.write().await = Some(runtime)`.
pub fn create_default_registry_with_spawn(
    workspace_root: impl AsRef<Path>,
    runtime_handle: RuntimeHandle,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let root = workspace_root.as_ref();

    // --- Core tools (read-only) ---
    registry.register(tools::read::ReadTool::new(root));
    registry.register(tools::glob::GlobTool::new(root));
    registry.register(tools::grep::GrepTool::new(root));

    // --- Mutation tools ---
    registry.register(tools::write::WriteTool::new(root));
    registry.register(tools::edit::EditTool::new(root));
    registry.register(tools::bash::BashTool::new(root));

    // --- KG primitive: recursive sub-agent spawning ---
    registry.register(tools::spawn::SpawnTool::new(root, runtime_handle));

    registry
}

/// Create a policy-scoped tool registry.
///
/// Only registers tools whose names appear in `allowed_tools`.
/// Used by operator containers to enforce policy at the tool registration level.
/// If a tool isn't registered, the LLM never sees it and can't call it.
pub fn create_policy_registry(workspace_root: impl AsRef<Path>, allowed_tools: &[&str]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let root = workspace_root.as_ref();

    for name in allowed_tools {
        match *name {
            "read" => registry.register(tools::read::ReadTool::new(root)),
            "glob" => registry.register(tools::glob::GlobTool::new(root)),
            "grep" => registry.register(tools::grep::GrepTool::new(root)),
            "write" => registry.register(tools::write::WriteTool::new(root)),
            "edit" => registry.register(tools::edit::EditTool::new(root)),
            "bash" => registry.register(tools::bash::BashTool::new(root)),
            _ => tracing::warn!("Unknown tool in policy: {}", name),
        }
    }

    registry
}
