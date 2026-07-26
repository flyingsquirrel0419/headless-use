//! Screenshot, annotation and dewiggle capture.
//!
//! Split out of `session/mod.rs`: these are pixel-capture operations that share
//! clip-rectangle math and have nothing to do with session lifecycle, input, or
//! reference resolution.

use std::time::Duration;

use serde_json::json;

use crate::browser::BrowserError;
use crate::session::{ClickTarget, Session};

/// Output of a `dewiggle` capture: a realigned, averaged PNG (base64) of the
/// wiggling text region, plus optional per-glyph crops.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DewiggleOutput {
    /// Realigned + averaged image, PNG, base64-encoded.
    pub image: String,
    /// Size of the PNG in bytes (before base64).
    pub image_bytes: usize,
    /// Per-glyph crops, PNG base64 (empty when `chars` was None).
    pub char_crops: Vec<String>,
    /// Number of frames captured.
    pub frame_count: usize,
    /// The captured region (viewport CSS px).
    pub region: serde_json::Value,
    /// Glyph column-band boundaries (start, end) in the output image.
    pub bands: Vec<(u32, u32)>,
}

impl Session {
    /// Capture a screenshot. When `element` is `Some`, only that element's
    /// region is captured (resolved to a clip rectangle via the current
    /// observation, scrolling into view first). When `full_page` is true the
    /// whole scrollable content is captured. The two are mutually exclusive:
    /// an element clip takes precedence over `full_page`.
    ///
    /// When tracing is enabled, the PNG is also saved into the run's
    /// `screenshots/` directory so it appears in `report.html`.
    pub async fn screenshot(
        &self,
        full_page: bool,
        element: Option<ClickTarget>,
    ) -> Result<Vec<u8>, BrowserError> {
        let clip = match &element {
            Some(target) => Some(self.element_clip(target.clone()).await?),
            None => None,
        };
        let element_ref = match &element {
            Some(ClickTarget::Ref { id, .. }) => Some(format!("@e{id}")),
            _ => None,
        };
        let seq = self
            .record_action(
                "screenshot",
                json!({ "fullPage": full_page, "element": element_ref }),
            )
            .await;
        let data = self.page.screenshot(full_page, clip).await?;
        // Save to trace so report.html can embed the image, using the same
        // sequence number as the action so the report can match them.
        let trace = self.trace.lock().unwrap().as_ref().cloned();
        if let Some(t) = trace {
            let _ = t.lock().await.save_screenshot(seq, &data).await;
        }
        Ok(data)
    }

    /// Capture a screenshot with bounding boxes + ref tokens drawn over every
    /// observed element. See `observe::annotate` for the rendering details.
    ///
    /// This re-runs observe (so the boxes match the current DOM, not a stale
    /// cache) and annotates the freshly captured PNG. Visual widgets (canvas,
    /// svg, cursor:pointer divs) are included so a vision agent can locate
    /// non-standard clickable surfaces too.
    pub async fn screenshot_annotated(&self, full_page: bool) -> Result<Vec<u8>, BrowserError> {
        let obs = self.observe_with_mode(None).await?;
        let png = self.page.screenshot(full_page, None).await?;
        let vp = crate::cdp::Viewport {
            width: obs.page.viewport.width,
            height: obs.page.viewport.height,
            device_scale_factor: obs.page.viewport.device_scale_factor,
        };
        crate::observe::annotate::annotate(&png, &obs.elements, &vp)
            .map_err(|e| BrowserError::Decode(format!("annotate: {e}")))
    }

    /// Capture several frames of a wiggling/animated text region and reverse
    /// the per-glyph vertical wobble using *pixels only* (no answer arrays,
    /// no DOM text/props). Returns a realigned, averaged PNG plus optional
    /// per-glyph crops. This is the honest way to read a "wiggle" CAPTCHA.
    ///
    /// When `region` is None, the canvas element's bounding box is auto-
    /// detected via `document.querySelector('canvas')`. When `chars` is Some,
    /// the output is also segmented into that many equal-width glyph crops.
    pub async fn dewiggle(
        &self,
        region: Option<(f64, f64, f64, f64)>,
        frames: u32,
        interval_ms: u64,
        chars: Option<usize>,
    ) -> Result<DewiggleOutput, BrowserError> {
        let frames = frames.clamp(2, 60);
        let interval_ms = interval_ms.clamp(20, 2000);

        // NOTE: We do NOT start a screencast here. In `view` mode the viewer
        // already runs one (which pumps the rAF loop), and starting a second
        // screencast crashes Chrome. In `serve` mode there is no screencast,
        // but the dewiggle integration test is #[ignore]d for that reason.
        // `Page.captureScreenshot(clip)` forces a composite render regardless,
        // so the captures themselves work without a screencast.

        let inner = async {
            // Resolve the capture region (viewport CSS px). Auto-detect a canvas.
            let (vx, vy, vw, vh) = match region {
                Some(r) => r,
                None => {
                    // Detect the canvas via the DOM domain (no JS evaluation).
                    // A perpetual rAF loop makes Runtime.evaluate hang in headless
                    // mode, so we avoid JS entirely for region detection.
                    match self.page.element_box_by_selector("canvas").await? {
                        Some((x, y, w, h)) => (x, y, w, h),
                        None => {
                            return Err(BrowserError::InvalidInput(
                                "dewiggle: no canvas found and no region given; pass region=[x,y,w,h]".into(),
                            ));
                        }
                    }
                }
            };
            if vw < 2.0 || vh < 2.0 {
                return Err(BrowserError::InvalidInput(format!(
                    "dewiggle region too small: {vw}x{vh}"
                )));
            }

            let seq = self
                .record_action(
                    "dewiggle",
                    json!({ "region": [vx, vy, vw, vh], "frames": frames, "intervalMs": interval_ms, "chars": chars }),
                )
                .await;
            let _ = seq;

            // Capture N frames at interval_ms cadence. Clip is document-relative,
            // so add the scroll offset.
            let (sx, sy) = self.page.scroll_position().await?;
            let clip = (vx + sx as f64, vy + sy as f64, vw, vh);
            let mut dewiggle_frames: Vec<crate::observe::dewiggle::DewiggleFrame> = Vec::new();
            for i in 0..frames {
                if i > 0 {
                    tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                }
                let png = self.page.screenshot(false, Some(clip)).await?;
                let gray = crate::observe::dewiggle::decode_gray(&png)
                    .map_err(|e| BrowserError::Decode(format!("dewiggle frame: {e}")))?;
                dewiggle_frames.push(crate::observe::dewiggle::DewiggleFrame::new(gray));
            }

            let opts = crate::observe::dewiggle::DewiggleOptions {
                chars,
                ..Default::default()
            };
            let result = crate::observe::dewiggle::dewiggle(&dewiggle_frames, &opts);
            let image_png = crate::observe::dewiggle::encode_png(&result.image)
                .map_err(|e| BrowserError::Decode(format!("dewiggle output: {e}")))?;
            let char_crops_b64: Vec<String> = result
                .char_crops
                .iter()
                .map(|c| {
                    crate::observe::dewiggle::encode_png(c)
                        .map(|b| {
                            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &b)
                        })
                        .unwrap_or_default()
                })
                .collect();

            let image_b64 =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image_png);
            Ok::<DewiggleOutput, BrowserError>(DewiggleOutput {
                image: image_b64,
                image_bytes: image_png.len(),
                char_crops: char_crops_b64,
                frame_count: result.frame_count,
                region: json!({ "x": vx, "y": vy, "width": vw, "height": vh }),
                bands: result.bands,
            })
        };

        let result = inner.await;
        result
    }

    /// Compute the `Page.captureScreenshot` clip rectangle for a target, in
    /// **document** coordinates.
    ///
    /// Two things this has to get right:
    ///
    /// 1. `getBoundingClientRect()` is viewport-relative but CDP's `clip` is
    ///    document-relative, so the page scroll offset must be added. Without
    ///    it, any element screenshot taken on a scrolled page captured the
    ///    wrong region. The offset is read *after* the element is scrolled into
    ///    view, because that scroll changes it.
    /// 2. For a `Ref` target we use the referenced element's own box. Going
    ///    through `elementFromPoint` at the element's center (the previous
    ///    behavior) returns the topmost *descendant* under that point — an
    ///    inner `<span>` rather than the `<button>` that was referenced — so
    ///    the clip came out too small. `elementFromPoint` is still the right
    ///    tool for a bare coordinate target, where there is no element to
    ///    resolve.
    async fn element_clip(
        &self,
        target: ClickTarget,
    ) -> Result<(f64, f64, f64, f64), BrowserError> {
        let (x, y, w, h) = match target {
            ClickTarget::Ref { id, generation } => {
                self.resolve_ref_rect_with_generation(id, generation)
                    .await?
            }
            ClickTarget::Point(p) => self.element_clip_at(p.x, p.y).await?,
        };
        let (scroll_x, scroll_y) = self.page.scroll_position().await?;
        Ok((x + scroll_x as f64, y + scroll_y as f64, w, h))
    }

    /// Compute a viewport-relative clip rectangle for the element under
    /// viewport point (x, y), via `elementFromPoint`.
    async fn element_clip_at(&self, x: f64, y: f64) -> Result<(f64, f64, f64, f64), BrowserError> {
        let expr = format!(
            "(()=>{{const e=document.elementFromPoint({x},{y});if(!e)return null;const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return null;return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
        );
        let s = self
            .page
            .evaluate_sync(&expr)
            .await?
            .value()
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        if s.is_empty() || s == "null" {
            return Err(BrowserError::ElementNotInteractable(format!(
                "no element at ({x:.0},{y:.0}) to screenshot"
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| BrowserError::Decode(format!("screenshot clip: {e}")))?;
        Ok((
            v.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0),
            v.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0),
        ))
    }
}
