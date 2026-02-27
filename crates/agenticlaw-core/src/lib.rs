//! Agenticlaw Core - Types, traits, and error handling

pub mod error;
pub mod types;
pub mod protocol;

pub use error::{Error, Result};
pub use types::*;
pub use protocol::*;
