//! Live viewer: stream the agent-controlled page over localhost as MJPEG,
//! plus inject a cursor overlay so the agent's CDP mouse input is visually
//! observable in real time.
//!
//! ## Why a viewer
//! On headless Linux / Xvfb there is no visible OS cursor for
//! `Input.dispatchMouseEvent`, so an agent's clicks and drags are invisible.
//! The viewer solves two things at once:
//!   1. injects a cursor overlay (see [`crate::browser::Page::inject_cursor_overlay`])
//!   2. streams frames via CDP `Page.startScreencast` to an MJPEG HTTP endpoint
//!      so a human (or a test) can watch the agent work live.
//!
//! The overlay source lives here (`cursor-overlay.js`) rather than under
//! `tests/`, because it ships inside the binary via `include_str!`.
//!
//! ## Security
//! The HTTP server binds to `127.0.0.1` by default. `--viewer-host` can widen
//! that for remote viewing, and [`http::serve`] warns whenever it does.
//!
//! Access is gated by a bearer token carried in the URL (`?token=…`), generated
//! at startup unless `--viewer-token` pins one. Enforcement depends on the bind
//! address: **required** on a non-loopback bind, **optional** on loopback —
//! generated, printed and accepted there, but a request that omits it is still
//! served. See [`http::ViewerOptions::require_token`] for why.
//!
//! The CDP endpoint is unaffected and always stays loopback-only.

pub mod http;
pub mod screencast;

pub use http::{ViewerHandle, ViewerOptions};
pub use screencast::Screencast;
