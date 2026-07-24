//! Live viewer: stream the agent-controlled page over localhost as MJPEG,
//! plus inject the neon-arrow cursor overlay so the agent's CDP mouse input
//! is visually observable in real time.
//!
//! ## Why a viewer
//! On headless Linux / Xvfb there is no visible OS cursor for
//! `Input.dispatchMouseEvent`, so an agent's clicks and drags are invisible.
//! The viewer solves two things at once:
//!   1. injects a cursor overlay (see [`crate::browser::Page::inject_cursor_overlay`])
//!   2. streams frames via CDP `Page.startScreencast` to a localhost-only MJPEG
//!      HTTP endpoint so a human (or a test) can watch the agent work live.
//!
//! ## Security
//! The HTTP server binds to `127.0.0.1` only — never `0.0.0.0` — to keep the
//! remote CDP-attached page off the network, consistent with the project rule
//! that CDP must never be exposed to other interfaces.

pub mod http;
pub mod screencast;

pub use http::{ViewerHandle, ViewerOptions};
pub use screencast::Screencast;
