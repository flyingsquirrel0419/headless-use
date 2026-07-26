//! Shared input types: modifiers, points, button bitmasks, cursor motion.

use std::time::Duration;

/// How the cursor travels to a click/hover target.
///
/// ## Why this is a setting and not a constant
/// Interpolating the path dispatches a burst of real `mouseMoved` events, so
/// the page sees the cursor travel: hover menus open, sliders track, and the
/// live viewer shows motion instead of a teleport. It also costs the travel
/// time on every click, which is dead weight for an agent running headless in
/// CI. So the *watching* surface (`headless-use view`) defaults to
/// [`CursorMotion::Smooth`] and the automation surfaces default to
/// [`CursorMotion::Instant`]; `--cursor-motion` overrides either way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorMotion {
    /// Jump straight to the target: one `mouseMoved` event, no delay.
    Instant,
    /// Ease along the path, emitting intermediate `mouseMoved` events.
    Smooth {
        /// Total travel time.
        duration: Duration,
        /// Number of interpolation steps.
        steps: u32,
    },
}

impl CursorMotion {
    /// The default smooth profile: 220ms over 24 steps.
    ///
    /// 220/24 is ~9.2ms per step, just above the 8ms floor in
    /// [`crate::input::Mouse::move_smooth`] below which Chrome's input queue
    /// stutters.
    pub const fn smooth_default() -> Self {
        Self::Smooth {
            duration: Duration::from_millis(220),
            steps: 24,
        }
    }

    /// True if this profile interpolates.
    pub fn is_smooth(self) -> bool {
        matches!(self, Self::Smooth { .. })
    }

    /// Parse the `--cursor-motion` flag value.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "instant" | "snap" | "off" => Ok(Self::Instant),
            "smooth" | "on" => Ok(Self::smooth_default()),
            other => Err(format!(
                "unknown cursor motion '{other}'. Allowed: smooth, instant"
            )),
        }
    }
}

impl Default for CursorMotion {
    /// [`CursorMotion::Instant`], so a library caller that does not opt in pays
    /// no travel time.
    fn default() -> Self {
        Self::Instant
    }
}

/// Modifier key bitmask, matching CDP / DOM conventions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(pub u8);

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// Alt.
    pub const ALT: Self = Self(1);
    /// Ctrl.
    pub const CTRL: Self = Self(2);
    /// Meta/Command.
    pub const META: Self = Self(4);
    /// Shift.
    pub const SHIFT: Self = Self(8);

    /// Combine two modifier sets.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// True if any modifier is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for Modifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.0 & Self::ALT.0 != 0 {
            parts.push("alt");
        }
        if self.0 & Self::CTRL.0 != 0 {
            parts.push("ctrl");
        }
        if self.0 & Self::META.0 != 0 {
            parts.push("meta");
        }
        if self.0 & Self::SHIFT.0 != 0 {
            parts.push("shift");
        }
        if parts.is_empty() {
            write!(f, "none")
        } else {
            write!(f, "{}", parts.join("+"))
        }
    }
}

/// Parse a comma-separated modifier list like `ctrl,shift` into a bitmask.
pub fn parse_modifiers(s: &str) -> Result<Modifiers, String> {
    let mut mods = Modifiers::NONE;
    for part in s.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "alt" | "option" => mods = mods.union(Modifiers::ALT),
            "ctrl" | "control" | "cmd" if false => {}
            "ctrl" | "control" => mods = mods.union(Modifiers::CTRL),
            "meta" | "cmd" | "command" => mods = mods.union(Modifiers::META),
            "shift" => mods = mods.union(Modifiers::SHIFT),
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

/// A 2D point in viewport CSS pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
}

impl Point {
    /// New point.
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl From<(f64, f64)> for Point {
    fn from((x, y): (f64, f64)) -> Self {
        Self { x, y }
    }
}

/// Parse a coordinate pair like `400,300` or `400 300`.
pub fn parse_point(s: &str) -> Result<Point, String> {
    let s = s.trim().replace(',', " ");
    let mut it = s.split_whitespace();
    let x: f64 = it
        .next()
        .ok_or_else(|| "expected x coordinate".to_string())?
        .parse()
        .map_err(|_| format!("invalid x in '{s}'"))?;
    let y: f64 = it
        .next()
        .ok_or_else(|| "expected y coordinate".to_string())?
        .parse()
        .map_err(|_| format!("invalid y in '{s}'"))?;
    Ok(Point { x, y })
}

/// Parse a list of points for drag-path: `[[100,200],[300,400]]`.
pub fn parse_point_list(s: &str) -> Result<Vec<Point>, String> {
    let v: Vec<Vec<f64>> =
        serde_json::from_str(s).map_err(|e| format!("invalid point list '{s}': {e}"))?;
    v.into_iter()
        .map(|pair| {
            if pair.len() != 2 {
                return Err("each point must be [x, y]".into());
            }
            Ok(Point {
                x: pair[0],
                y: pair[1],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_parse() {
        assert_eq!(
            parse_modifiers("ctrl,shift").unwrap(),
            Modifiers::CTRL.union(Modifiers::SHIFT)
        );
        assert_eq!(parse_modifiers("Shift").unwrap(), Modifiers::SHIFT);
        assert_eq!(parse_modifiers("cmd").unwrap(), Modifiers::META);
        assert_eq!(parse_modifiers("").unwrap(), Modifiers::NONE);
        assert!(parse_modifiers("foobar").is_err());
    }

    #[test]
    fn point_parse() {
        let p = parse_point("400,300").unwrap();
        assert_eq!(p.x, 400.0);
        assert_eq!(p.y, 300.0);
        let p2 = parse_point("100 200").unwrap();
        assert_eq!(p2.x, 100.0);
        assert_eq!(p2.y, 200.0);
    }

    #[test]
    fn point_list_parse() {
        let pts = parse_point_list("[[100,200],[300,400]]").unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[1].x, 300.0);
    }

    #[test]
    fn cursor_motion_parse() {
        assert_eq!(
            CursorMotion::parse("instant").unwrap(),
            CursorMotion::Instant
        );
        assert_eq!(
            CursorMotion::parse("  INSTANT ").unwrap(),
            CursorMotion::Instant
        );
        assert_eq!(
            CursorMotion::parse("smooth").unwrap(),
            CursorMotion::smooth_default()
        );
        assert!(CursorMotion::parse("fast").is_err());
        // Default is the cheap one: a library caller pays nothing unless it
        // opts in.
        assert_eq!(CursorMotion::default(), CursorMotion::Instant);
        assert!(!CursorMotion::Instant.is_smooth());
        assert!(CursorMotion::smooth_default().is_smooth());
    }

    /// The smooth profile must stay above `move_smooth`'s 8ms per-step floor,
    /// below which Chrome's input queue stutters.
    #[test]
    fn smooth_default_step_interval_is_above_the_floor() {
        let CursorMotion::Smooth { duration, steps } = CursorMotion::smooth_default() else {
            panic!("smooth_default must be Smooth");
        };
        let per_step = duration.as_secs_f64() * 1000.0 / steps as f64;
        assert!(
            per_step >= 8.0,
            "step interval {per_step}ms is below the 8ms floor"
        );
    }
}
