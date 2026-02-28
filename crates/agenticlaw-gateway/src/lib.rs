//! Rustclaw Gateway - WebSocket server, TUI, and full agent runtime

pub mod auth;
pub mod rpc;
pub mod server;
pub mod service;
pub mod tui;
pub mod tui_client;
pub mod ws;

pub use server::{start_gateway, ExtendedConfig};
