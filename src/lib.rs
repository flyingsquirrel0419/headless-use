//! headless-use: Computer use for web development agents, built for headless Linux and CI.
//!
//! This crate exposes a layered browser runtime:
//!   1. [`browser`] — process management + CDP transport
//!   2. [`input`] — real `Input.*` CDP input events (mouse, keyboard, drag)
//!   3. [`observe`] — accessibility/DOM observation + semantic references
//!   4. [`session`] — long-lived sessions, tabs, navigation
//!   5. [`trace`] — action recording + report + replay
//!   6. [`protocol`] — JSON-RPC over stdio
//!   7. [`mcp`] — Model Context Protocol tool definitions

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::dbg_macro, clippy::todo, clippy::unimplemented)]

pub mod browser;
pub mod cdp;
pub mod cli;
pub mod input;
pub mod mcp;
pub mod observe;
pub mod protocol;
pub mod security;
pub mod session;
pub mod trace;
pub mod util;

pub use browser::{Browser, BrowserError, LaunchOptions};

/// Crate version, set at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
