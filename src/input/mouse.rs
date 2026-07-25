//! Mouse input engine via CDP `Input.dispatchMouseEvent`.
//!
//! Dispatches real `mouseMoved`/`mousePressed`/`mouseReleased`/`mouseWheel`
//! events. We track the current cursor position and pressed buttons in a
//! per-page session so drag sequences and hover states are consistent.
//!
//! ## Why interpolation
//! Many sites (sliders, canvases, maps, sortable lists) listen for intermediate
//! `mousemove` events and break if you teleport from A to B. Drag therefore
//! emits `mouseMoved` along a linear path with enough steps to be realistic.

use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{BrowserError, Page};
use crate::input::types::{Modifiers, Point};

/// CDP mouse button string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// No button — used for plain cursor moves and wheel scrolls.
    /// CDP requires `button: "none"` when no button is pressed, otherwise
    /// the page's JS sees `buttons: 1` (left held) on every mouseMoved.
    None,
    /// Left button (primary).
    Left,
    /// Middle button (auxiliary).
    Middle,
    /// Right button (secondary).
    Right,
    /// Back button.
    Back,
    /// Forward button.
    Forward,
}

impl MouseButton {
    /// CDP string for the `button` field.
    pub fn as_cdp(self) -> &'static str {
        match self {
            MouseButton::None => "none",
            MouseButton::Left => "left",
            MouseButton::Middle => "middle",
            MouseButton::Right => "right",
            MouseButton::Back => "back",
            MouseButton::Forward => "forward",
        }
    }

    /// Bitmask for the `buttons` field (which buttons are held).
    pub fn bitmask(self) -> u8 {
        match self {
            MouseButton::None => 0,
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Back => 8,
            MouseButton::Forward => 16,
        }
    }

    /// Parse from a string for public API use.
    /// "none" is internal-only (for CDP moves) and rejected here so that
    /// typos like "rgiht" don't silently fall back to left.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" => Ok(MouseButton::Left),
            "left" | "l" => Ok(MouseButton::Left),
            "middle" | "m" => Ok(MouseButton::Middle),
            "right" | "r" => Ok(MouseButton::Right),
            "back" => Ok(MouseButton::Back),
            "forward" => Ok(MouseButton::Forward),
            other => Err(format!(
                "unknown mouse button '{other}'. Allowed: left, middle, right, back, forward"
            )),
        }
    }
}

/// Mouse engine bound to a page. Cheap to clone; shares cursor state via the
/// page's session-scoped CDP client.
#[derive(Clone)]
pub struct Mouse<'a> {
    page: &'a Page,
    pos: std::sync::Arc<tokio::sync::Mutex<Point>>,
    buttons: std::sync::Arc<tokio::sync::Mutex<u8>>,
}

impl<'a> Mouse<'a> {
    /// Create a mouse engine for `page` with fresh cursor state.
    pub fn new(page: &'a Page) -> Self {
        Self {
            page,
            pos: std::sync::Arc::new(tokio::sync::Mutex::new(Point::new(0.0, 0.0))),
            buttons: std::sync::Arc::new(tokio::sync::Mutex::new(0)),
        }
    }

    /// Create a mouse engine sharing existing cursor/button state (for Session).
    /// This is the critical fix: without shared state, a sequence like
    /// `move_to(300,200)` then `down()` then `up()` would create three
    /// independent Mouse instances, each thinking the cursor is at (0,0).
    pub fn new_with_state(
        page: &'a Page,
        pos: std::sync::Arc<tokio::sync::Mutex<Point>>,
        buttons: std::sync::Arc<tokio::sync::Mutex<u8>>,
    ) -> Self {
        Self { page, pos, buttons }
    }

    /// Current cursor position.
    pub async fn position(&self) -> Point {
        *self.pos.lock().await
    }

    /// Move the cursor instantly. Uses `button: "none"` so the page does not
    /// interpret a plain move as a left-button-held move.
    pub async fn move_to(&self, p: Point, mods: Modifiers) -> Result<(), BrowserError> {
        let buttons = *self.buttons.lock().await;
        self.dispatch(
            "mouseMoved",
            p,
            MouseButton::None,
            buttons,
            mods,
            None,
            None,
        )
        .await?;
        *self.pos.lock().await = p;
        Ok(())
    }

    /// Move the cursor from its current position to `p` over `duration`, in
    /// `steps` interpolated steps.
    ///
    /// Uses an ease-in-out curve (smooth start and end, faster in the middle)
    /// plus a slight perpendicular bow so the path looks like a natural human
    /// hand movement rather than a straight teleport. This matters for the
    /// live viewer: a linear, instant snap looks robotic and is hard to follow
    /// by eye.
    pub async fn move_smooth(
        &self,
        p: Point,
        mods: Modifiers,
        duration: Duration,
        steps: u32,
    ) -> Result<(), BrowserError> {
        if steps == 0 {
            return self.move_to(p, mods).await;
        }
        let start = self.position().await;
        let step_dur = if duration.as_millis() > 0 {
            duration / steps.max(1)
        } else {
            Duration::from_millis(0)
        };
        let buttons = *self.buttons.lock().await;
        // Perpendicular bow magnitude: a small fraction of the travel
        // distance, capped so very short moves stay near-linear.
        let dx = p.x - start.x;
        let dy = p.y - start.y;
        let dist = (dx * dx + dy * dy).sqrt();
        let bow = (dist * 0.12).min(120.0);
        // Perpendicular unit vector (rotated 90deg). If dist is ~0 the bow
        // is ~0 anyway, so the direction doesn't matter.
        let (px, py) = if dist > 0.0001 {
            (-dy / dist, dx / dist)
        } else {
            (0.0, 0.0)
        };
        for i in 1..=steps {
            let lin = i as f64 / steps as f64;
            // Ease-in-out: slow at the ends, fast in the middle. This makes
            // the cursor visibly accelerate then decelerate instead of moving
            // at a constant linear speed.
            let eased = if lin < 0.5 {
                2.0 * lin * lin
            } else {
                1.0 - ((-2.0 * lin + 2.0).powi(2)) / 2.0
            };
            // Quadratic bow: the curve peaks at t=0.5 and returns to 0 at
            // the endpoints, so start and end land exactly on the straight
            // line (the cursor still ends at `p`).
            let bow_t = 4.0 * lin * (1.0 - lin);
            let x = start.x + dx * eased + px * bow * bow_t;
            let y = start.y + dy * eased + py * bow * bow_t;
            self.dispatch(
                "mouseMoved",
                Point { x, y },
                MouseButton::None,
                buttons,
                mods,
                None,
                None,
            )
            .await?;
            if !step_dur.is_zero() {
                tokio::time::sleep(step_dur).await;
            }
        }
        *self.pos.lock().await = p;
        Ok(())
    }

    /// Press a button down at the current position.
    /// Accumulates into the shared buttons bitmask so multi-button states
    /// (e.g. left+right) are correctly reported.
    ///
    /// Transactional: the shared button state is committed only after the CDP
    /// dispatch succeeds. If dispatch fails, Rust state stays consistent with
    /// the browser (the button is NOT recorded as held locally).
    pub async fn down(&self, button: MouseButton, mods: Modifiers) -> Result<(), BrowserError> {
        let p = self.position().await;
        // Compute the target cumulative buttons state without committing yet.
        let target_buttons = {
            let b = self.buttons.lock().await;
            *b | button.bitmask()
        };
        self.dispatch(
            "mousePressed",
            p,
            button,
            target_buttons,
            mods,
            Some(1),
            None,
        )
        .await?;
        // Commit only after a successful dispatch.
        *self.buttons.lock().await = target_buttons;
        Ok(())
    }

    /// Release a button at the current position.
    /// Removes from the shared buttons bitmask so the remaining held buttons
    /// are still reported (e.g. release right while left is still held).
    ///
    /// Transactional: the shared button state is committed only after the CDP
    /// dispatch succeeds, so a failed release does not leave local state
    /// inconsistent with the browser.
    pub async fn up(&self, button: MouseButton, mods: Modifiers) -> Result<(), BrowserError> {
        let p = self.position().await;
        // Compute the target cumulative buttons state without committing yet.
        let target_buttons = {
            let b = self.buttons.lock().await;
            *b & !button.bitmask()
        };
        self.dispatch(
            "mouseReleased",
            p,
            button,
            target_buttons,
            mods,
            Some(1),
            None,
        )
        .await?;
        // Commit only after a successful dispatch.
        *self.buttons.lock().await = target_buttons;
        Ok(())
    }

    /// Click (press + release) at `p`, optionally with count for double/triple.
    pub async fn click(
        &self,
        p: Point,
        button: MouseButton,
        count: u32,
        mods: Modifiers,
        hold: Duration,
    ) -> Result<(), BrowserError> {
        if count == 0 {
            return Err(BrowserError::InvalidInput("click count must be > 0".into()));
        }
        self.move_to(p, mods).await?;
        for i in 1..=count {
            // Accumulate button state: press adds, release removes.
            // This ensures that clicking right while left is held reports
            // buttons=3 (down) then buttons=1 (up), not 2 then 0.
            // Transactional: commit each state change only after its dispatch
            // succeeds, so a failure mid-click leaves local state consistent.
            let pressed_buttons = {
                let b = self.buttons.lock().await;
                *b | button.bitmask()
            };
            self.dispatch(
                "mousePressed",
                p,
                button,
                pressed_buttons,
                mods,
                Some(i),
                None,
            )
            .await?;
            *self.buttons.lock().await = pressed_buttons;
            if !hold.is_zero() {
                tokio::time::sleep(hold).await;
            }
            let released_buttons = {
                let b = self.buttons.lock().await;
                *b & !button.bitmask()
            };
            self.dispatch(
                "mouseReleased",
                p,
                button,
                released_buttons,
                mods,
                Some(i),
                None,
            )
            .await?;
            *self.buttons.lock().await = released_buttons;
        }
        Ok(())
    }

    /// Scroll by (`dx`, `dy`) at an optional `at` point. Uses `mouseWheel`.
    pub async fn scroll(
        &self,
        dx: f64,
        dy: f64,
        at: Option<Point>,
        mods: Modifiers,
        duration: Duration,
        steps: u32,
    ) -> Result<(), BrowserError> {
        let at = match at {
            Some(p) => p,
            None => self.position().await,
        };
        // Wheel uses button="none" but preserves the current buttons bitmask
        // so scrolling while holding a button reports the correct state.
        let current_buttons = *self.buttons.lock().await;
        let steps = steps.max(1);
        let per_step_x = dx / steps as f64;
        let per_step_y = dy / steps as f64;
        let step_dur = if duration.as_millis() > 0 {
            duration / steps
        } else {
            Duration::from_millis(0)
        };
        for _ in 0..steps {
            self.dispatch(
                "mouseWheel",
                at,
                MouseButton::None,
                current_buttons,
                mods,
                None,
                Some((per_step_x, per_step_y)),
            )
            .await?;
            if !step_dur.is_zero() {
                tokio::time::sleep(step_dur).await;
            }
        }
        Ok(())
    }

    /// Drag from `from` to `to`, emitting intermediate moves (move, down, move*N, up).
    pub async fn drag(
        &self,
        from: Point,
        to: Point,
        button: MouseButton,
        mods: Modifiers,
        duration: Duration,
        steps: u32,
    ) -> Result<(), BrowserError> {
        if steps == 0 {
            return Err(BrowserError::InvalidInput("drag steps must be > 0".into()));
        }
        self.move_to(from, mods).await?;
        self.down(button, mods).await?;
        let step_dur = if duration.as_millis() > 0 {
            duration / steps
        } else {
            Duration::from_millis(0)
        };
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let x = from.x + (to.x - from.x) * t;
            let y = from.y + (to.y - from.y) * t;
            let p = Point { x, y };
            // Use the cumulative held-buttons mask (which includes the dragged
            // button pressed in down() above plus any other held buttons) so
            // intermediate moves report the correct `buttons` value. Using just
            // `button.bitmask()` would drop already-held buttons (e.g. right
            // held + left drag would report buttons=1 instead of 3 mid-drag).
            // The `button` field uses the dragged button (not None) so Chrome's
            // pointer-capture for native controls (range slider, etc.) stays
            // active and pointermove events fire during the drag.
            let held = *self.buttons.lock().await;
            self.dispatch("mouseMoved", p, button, held, mods, None, None)
                .await?;
            // Update tracked cursor position so up() releases at the correct spot.
            *self.pos.lock().await = p;
            if !step_dur.is_zero() {
                tokio::time::sleep(step_dur).await;
            }
        }
        // Ensure cursor position is exactly at destination before release.
        *self.pos.lock().await = to;
        self.up(button, mods).await?;
        Ok(())
    }

    /// Drag along a free path of points, emitting moves between each.
    pub async fn drag_path(
        &self,
        path: &[Point],
        button: MouseButton,
        mods: Modifiers,
        duration: Duration,
    ) -> Result<(), BrowserError> {
        if path.len() < 2 {
            return Err(BrowserError::InvalidInput(
                "drag-path needs at least 2 points".into(),
            ));
        }
        self.move_to(path[0], mods).await?;
        self.down(button, mods).await?;
        let seg_dur = if duration.as_millis() > 0 {
            duration / ((path.len() - 1) as u32)
        } else {
            Duration::from_millis(0)
        };
        for win in path.windows(2) {
            let from = win[0];
            let to = win[1];
            let steps = 8u32.max((seg_dur.as_millis() / 16) as u32);
            let inner = seg_dur / steps.max(1);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let x = from.x + (to.x - from.x) * t;
                let y = from.y + (to.y - from.y) * t;
                let p = Point { x, y };
                // Cumulative held-buttons mask so already-held buttons are not
                // dropped mid-drag (see drag() for the rationale). The `button`
                // field uses the dragged button so Chrome pointer-capture stays
                // active for native controls.
                let held = *self.buttons.lock().await;
                self.dispatch("mouseMoved", p, button, held, mods, None, None)
                    .await?;
                *self.pos.lock().await = p;
                if !inner.is_zero() {
                    tokio::time::sleep(inner).await;
                }
            }
        }
        *self.pos.lock().await = path[path.len() - 1];
        self.up(button, mods).await?;
        Ok(())
    }

    /// Low-level dispatch helper.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        kind: &str,
        p: Point,
        button: MouseButton,
        buttons: u8,
        mods: Modifiers,
        click_count: Option<u32>,
        wheel: Option<(f64, f64)>,
    ) -> Result<(), BrowserError> {
        let mut params = json!({
            "type": kind,
            "x": p.x,
            "y": p.y,
            "button": button.as_cdp(),
            "buttons": buttons,
            "modifiers": mods.0 as u64,
            "timestamp": now_monotonic(),
        });
        if let Some(c) = click_count {
            params["clickCount"] = json!(c);
        }
        if let Some((dx, dy)) = wheel {
            // deltaX/deltaY in CSS pixels; CDP expects device pixels but for
            // headless dsf=1 these coincide.
            params["deltaX"] = json!(dx);
            params["deltaY"] = json!(dy);
        }
        self.page
            .call::<Value>(
                "Input.dispatchMouseEvent",
                Some(params),
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }
}

/// CDP expects a monotonic timestamp in seconds since some epoch. Use a
/// process-local monotonic clock; Chrome only requires monotonic increase.
fn now_monotonic() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_parse_and_bitmask() {
        assert_eq!(MouseButton::parse("left").unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse("RIGHT").unwrap(), MouseButton::Right);
        assert_eq!(MouseButton::Left.bitmask(), 1);
        assert_eq!(MouseButton::Right.bitmask(), 2);
        assert_eq!(MouseButton::Middle.bitmask(), 4);
        assert!(MouseButton::parse("side").is_err());
    }

    #[test]
    fn linear_interpolation_endpoints() {
        // steps-based interpolation must start at `from` and end exactly at `to`.
        let steps = 4u32;
        let from = Point::new(0.0, 0.0);
        let to = Point::new(100.0, 200.0);
        let mut last = from;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            last = Point {
                x: from.x + (to.x - from.x) * t,
                y: from.y + (to.y - from.y) * t,
            };
        }
        assert_eq!(last.x, 100.0);
        assert_eq!(last.y, 200.0);
    }
}
