//! Observation engine: accessibility tree + DOM extraction + semantic references.
//!
//! The agent does not need full HTML. It needs a compact list of interactive
//! elements, each with a short stable reference (`@e1`, `@e2`, ...) and a
//! bounding box. This module builds that view from CDP's
//! `Accessibility.getFullAXTree` + `DOM` queries.
//!
//! ## Why references exist
//! Pure coordinate-based Computer Use forces the agent to screenshot on every
//! step to locate elements, burning tokens. References let the agent say
//! `click @e3` and let the runtime resolve the current location, scrolling and
//! checking visibility itself.

pub mod reference;

pub use reference::{
    parse_ref, parse_ref_with_generation, ElementRef, Observation, RefId, RefRegistry,
};

use serde_json::Value;

use crate::browser::{BrowserError, Page};

/// A semantic reference id, e.g. `e3` (without the leading `@`).
pub type RefIdValue = u32;

/// Build a fresh observation of the current page.
pub struct ObserveBuilder<'a> {
    page: &'a Page,
}

impl<'a> ObserveBuilder<'a> {
    /// Create a builder over `page`.
    pub fn new(page: &'a Page) -> Self {
        Self { page }
    }

    /// Run observe, extracting interactive elements and page metadata.
    pub async fn build(&self) -> Result<Observation, BrowserError> {
        self.page.enable("Accessibility.enable", None).await?;
        self.page.enable("DOM.enable", None).await?;

        let url = self.page.url().await?;
        let title = self.page.title().await?;
        let (sx, sy) = self.page.scroll_position().await?;

        // Query interactive elements + their AX info + boxes via one JS pass.
        // This is more reliable than stitching AX tree + DOM separately for the
        // common interactive set, and keeps it token-light.
        let script = include_str!("extract_elements.js");
        let result = self.page.evaluate(script).await?;

        let raw_elements = result
            .value()
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));

        let elements: Vec<ElementRef> = raw_elements
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| serde_json::from_value::<ElementRef>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let mut registry = RefRegistry::new();
        for el in &elements {
            registry.insert(el.clone());
        }

        let viewport = self.current_viewport().await?;

        let generation = registry.generation();
        Ok(Observation {
            page: PageMeta {
                url,
                title,
                viewport,
                scroll_x: sx,
                scroll_y: sy,
            },
            elements: registry.into_sorted(),
            generation,
            nav_generation: 0,
        })
    }

    async fn current_viewport(&self) -> Result<crate::cdp::Viewport, BrowserError> {
        let v = self
            .page
            .evaluate("JSON.stringify({w: window.innerWidth, h: window.innerHeight, d: window.devicePixelRatio})")
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "{\"w\":1280,\"h\":720,\"d\":1}".into());
        let obj: Value = serde_json::from_str(&v)
            .map_err(|e| BrowserError::Other(format!("viewport parse: {e}")))?;
        Ok(crate::cdp::Viewport {
            width: obj.get("w").and_then(|w| w.as_u64()).unwrap_or(1280) as u32,
            height: obj.get("h").and_then(|h| h.as_u64()).unwrap_or(720) as u32,
            device_scale_factor: obj.get("d").and_then(|d| d.as_f64()).unwrap_or(1.0),
        })
    }
}

/// Page-level metadata returned by observe.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PageMeta {
    /// Current URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Viewport.
    pub viewport: crate::cdp::Viewport,
    /// Horizontal scroll offset.
    pub scroll_x: i64,
    /// Vertical scroll offset.
    pub scroll_y: i64,
}
