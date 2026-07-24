//! Typed CDP domain types. Only the subset we actually use is modeled here.

use serde_json::Value;

/// A CDP `RemoteObject` (result of Runtime.evaluate etc.).
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct RemoteObject {
    /// Object type, e.g. `string`, `number`, `object`, `undefined`.
    #[serde(rename = "type", default)]
    pub object_type: String,
    /// Primitive value, present for string/number/boolean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Object id for reference types.
    #[serde(rename = "objectId", default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    /// Class name for objects.
    #[serde(rename = "className", default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A target (page, background_page, browser, ...) as reported by /json.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TargetInfo {
    /// Target id.
    #[serde(rename = "id")]
    pub id: String,
    /// Target type: `page`, `background_page`, `browser`, etc.
    #[serde(rename = "type")]
    pub target_type: String,
    /// Page title.
    #[serde(default)]
    pub title: String,
    /// Current URL.
    #[serde(default)]
    pub url: String,
    /// WebSocket URL for direct attachment.
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub ws_url: String,
}

impl TargetInfo {
    /// Parse a single JSON object into a TargetInfo.
    pub fn from_value(v: Value) -> Option<Self> {
        serde_json::from_value(v).ok()
    }

    /// True if this target is a page (not a background extension page).
    pub fn is_page(&self) -> bool {
        self.target_type == "page"
    }
}

/// Quadruple bounding box from DOM.getBoxModel / JS getBoundingClientRect.
#[derive(Debug, Clone, Copy, serde::Deserialize, Default)]
pub struct Quad {
    /// Four (x,y) points in order: top-left, top-right, bottom-right, bottom-left.
    #[serde(default)]
    pub quad: [f64; 8],
}

impl Quad {
    /// Create a quad from x,y,width,height.
    pub fn from_xywh(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            quad: [x, y, x + w, y, x + w, y + h, x, y + h],
        }
    }

    /// Axis-aligned bounding box (x, y, width, height).
    pub fn bbox(&self) -> (f64, f64, f64, f64) {
        let xs = [self.quad[0], self.quad[2], self.quad[4], self.quad[6]];
        let ys = [self.quad[1], self.quad[3], self.quad[5], self.quad[7]];
        let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// Center point (x, y) in CSS pixels.
    pub fn center(&self) -> (f64, f64) {
        let (x, y, w, h) = self.bbox();
        (x + w / 2.0, y + h / 2.0)
    }
}

/// Viewport dimensions reported by Emulation.setDeviceMetricsOverride result-free.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, Default)]
pub struct Viewport {
    /// Width in CSS pixels.
    pub width: u32,
    /// Height in CSS pixels.
    pub height: u32,
    /// Device scale factor.
    #[serde(rename = "deviceScaleFactor", default = "one")]
    pub device_scale_factor: f64,
}

fn one() -> f64 {
    1.0
}

impl Viewport {
    /// Parse a `WxH` string like `1280x720`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (w, h) = s
            .split_once('x')
            .ok_or_else(|| format!("invalid viewport '{s}', expected WxH"))?;
        let width: u32 = w
            .trim()
            .parse()
            .map_err(|_| format!("invalid width '{w}'"))?;
        let height: u32 = h
            .trim()
            .parse()
            .map_err(|_| format!("invalid height '{h}'"))?;
        if width == 0 || height == 0 {
            return Err("viewport dimensions must be > 0".into());
        }
        Ok(Self {
            width,
            height,
            device_scale_factor: 1.0,
        })
    }
}

impl std::fmt::Display for Viewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}
