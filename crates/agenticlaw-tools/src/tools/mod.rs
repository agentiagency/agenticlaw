//! Individual tool implementations.
//!
//! Each tool is a self-contained module. To add a new tool:
//! 1. Create a new file in this directory
//! 2. Implement the Tool trait
//! 3. Add `pub mod <name>;` here
//! 4. Register it in create_default_registry() in ../lib.rs

pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod spawn;
pub mod subagent;
pub mod write;
