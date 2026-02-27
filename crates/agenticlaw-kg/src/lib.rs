//! agenticlaw-kg — Knowledge Graph Executor
//!
//! Observable agent tree execution with pluggable resource backends.
//! The executor is code (structural scaffolding); agents do the thinking.

pub mod executor;
pub mod manifest;
pub mod persistent;
pub mod registry;
pub mod resource;

pub use executor::{Executor, RunConfig, NodePrep};
pub use manifest::{RunManifest, Outcome, NodeState};
pub use persistent::{IssueNode, PrNode};
pub use registry::{NodeTypeRegistry, NodeType, OperatorRole, TemplateVars, default_issue_registry, render_template_pub};
pub use resource::{GraphAddress, ResourceDriver, LocalFsDriver, KgEvent, Artifact};
