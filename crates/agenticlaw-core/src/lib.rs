//! Rustclaw Core - Types, traits, error handling, and config

pub mod error;
pub mod types;
pub mod protocol;
pub mod openclaw_config;

pub use error::{Error, Result};
pub use types::*;
pub use protocol::*;
pub use openclaw_config::OpenclawConfig;
