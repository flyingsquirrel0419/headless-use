//! Chrome DevTools Protocol (CDP) transport layer.
//!
//! This module is the single place that knows about raw CDP JSON. Higher layers
//! call [`CdpClient`] methods and receive typed Rust results; raw CDP messages
//! are intentionally not leaked past this boundary.
//!
//! ## Why
//! CDP is a verbose, deeply nested JSON-RPC protocol. Exposing raw CDP dicts
//! upward would force every caller to re-parse the same fields and would make
//! error handling inconsistent. Instead we map CDP to typed Rust and to the
//! structured [`crate::BrowserError`] enum so the agent layer always knows what
//! kind of recovery is possible.

pub mod client;
pub mod error;
pub mod types;

pub use client::{CdpClient, CdpEvent, EvaluateResult};
pub use error::CdpError;
pub use types::*;
