#![allow(
    dead_code,
    clippy::should_implement_trait,
    clippy::large_enum_variant,
    clippy::doc_lazy_continuation
)]

pub mod supervisor;

// These modules are shared between the main binary and lib for testing
pub mod context;
pub mod format;
pub mod openclaw;
pub mod parser;
pub mod session;
pub mod transform;
pub mod types;
