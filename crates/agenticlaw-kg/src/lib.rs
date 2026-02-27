//! agenticlaw-kg — Knowledge Graph Executor
//!
//! Observable agent tree execution with pluggable resource backends.
//! The executor is code (structural scaffolding); agents do the thinking.

pub mod executor;
pub mod manifest;
pub mod persistent;
pub mod registry;
pub mod resource;

pub use executor::{Executor, NodePrep, RunConfig};
pub use manifest::{NodeState, Outcome, RunManifest};
pub use persistent::{IssueNode, PrNode};
pub use registry::{
    default_issue_registry, render_template_pub, NodeType, NodeTypeRegistry, OperatorRole,
    TemplateVars,
};
pub use resource::{Artifact, GraphAddress, KgEvent, LocalFsDriver, ResourceDriver};
