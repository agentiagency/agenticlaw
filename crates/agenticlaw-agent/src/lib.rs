//! Agenticlaw Agent - Runtime for tool-using AI agents with .ctx persistence

pub mod context;
pub mod ctx_file;
pub mod queue;
pub mod runtime;
pub mod session;

pub use context::ContextManager;
pub use queue::{
    ConsciousnessLoop, ConsciousnessLoopConfig, OutputEvent, Priority, QueueEvent, ToolHandle,
    ToolState,
};
pub use runtime::{AgentConfig, AgentEvent, AgentRuntime};
pub use session::{Session, SessionKey, SessionRegistry};
