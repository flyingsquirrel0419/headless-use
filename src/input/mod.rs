//! Input engine: real CDP `Input.*` events.
//!
//! ## Why real input events (non-negotiable)
//! Calling `element.click()` from JS does not reproduce what a real user does:
//! it bypasses pointer events, hit-testing, and is detectable by sites. We
//! instead dispatch `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` /
//! `Input.insertText`, which produce the same event sequence a physical input
//! device would.
//!
//! ## Coordinate system
//! All coordinates are **viewport CSS pixels** with origin at the top-left of
//! the viewport. `Input.dispatchMouseEvent` with `x`/`y` uses this same space.

pub mod keyboard;
pub mod mouse;
pub mod types;

pub use keyboard::{Key, Keyboard};
pub use mouse::{Mouse, MouseButton};
pub use types::{parse_modifiers, parse_point, parse_point_list, Modifiers, Point};

use crate::browser::Page;

/// Construct the input engine for a page.
pub fn for_page(page: &Page) -> Input<'_> {
    Input {
        keyboard: Keyboard::new(page),
        mouse: Mouse::new(page),
    }
}

/// Bundled keyboard + mouse engine.
#[derive(Clone)]
pub struct Input<'a> {
    /// Keyboard engine.
    pub keyboard: Keyboard<'a>,
    /// Mouse engine.
    pub mouse: Mouse<'a>,
}
