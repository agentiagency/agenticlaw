//! Agenticlaw Consciousness — dual-core cascading consciousness stack
//!
//! Architecture:
//! - L0 (Gateway): User-facing agent on port 18789, full tool access
//! - L1 (Attention): Watches L0's .ctx, distills what matters now
//! - L2 (Pattern): Watches L1's .ctx, finds recurring themes
//! - L3 (Integration): Watches L2's .ctx, synthesizes understanding
//! - Core-A / Core-B: Phase-locked dual cores watching L3, maintain identity
//!
//! Trigger: FILE CHANGE on .ctx size, not time intervals.
//! Injection: Lower layers append insights to L0's context when correlated.

pub mod config;
pub mod cores;
pub mod ego;
pub mod injection;
pub mod stack;
pub mod version;
pub mod watcher;
