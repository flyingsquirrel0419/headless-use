//! Semantic references and the registry that maps `@eN` -> element location.
//!
//! ## Reference lifecycle (why)
//! After a page mutates, an old `@e3` may point at a moved or removed element.
//! We assign references per `observe` generation and detect staleness by
//! comparing the generation stored on the reference against the current page
//! generation. References are re-resolved to *current* coordinates at action
//! time (not cached), so a still-existing element remains clickable across
//! layout shifts within the same generation.

use std::collections::BTreeMap;

/// A short semantic reference id, e.g. `3` for `@e3`.
pub type RefId = u32;

/// One observed interactive element.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementRef {
    /// Reference number (`@eN` -> N). Defaults to 0 until the registry assigns it.
    #[serde(default)]
    pub ref_id: RefId,
    /// ARIA/DOM role.
    pub role: String,
    /// Accessible name / label.
    pub name: String,
    /// Tag name.
    #[serde(rename = "tagName")]
    pub tag_name: String,
    /// CSS-pixel x (left).
    pub x: i64,
    /// CSS-pixel y (top).
    pub y: i64,
    /// Width in CSS pixels.
    pub width: i64,
    /// Height in CSS pixels.
    pub height: i64,
    /// Whether the element is visible.
    pub visible: bool,
    /// Whether the element is enabled (not disabled).
    pub enabled: bool,
    /// Whether the element is focused.
    pub focused: bool,
    /// Checked state (for checkboxes/radios), else null.
    pub checked: Option<bool>,
    /// Current value (for inputs), else null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Best-effort CSS selector for re-resolution.
    #[serde(
        rename = "selectorHint",
        skip_serializing_if = "String::is_empty",
        default
    )]
    pub selector_hint: String,
    /// Whether this element is a non-standard "visual widget" (canvas, svg,
    /// cursor:pointer div, etc.) captured by the heuristic second pass rather
    /// than the standard interactive selector. Interactive-only observe modes
    /// exclude these; annotate renders them in a different color.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub visual: bool,
    /// The canonical reference token (`@g<gen>:e<num>`), populated after the
    /// registry assigns a generation. Agents should use this token directly for
    /// clicks/hovers so the generation is always carried for stale detection.
    #[serde(rename = "ref", skip_serializing_if = "String::is_empty", default)]
    pub ref_token: String,
}

impl ElementRef {
    /// Center point (x, y) in CSS pixels.
    pub fn center(&self) -> (f64, f64) {
        (
            self.x as f64 + self.width as f64 / 2.0,
            self.y as f64 + self.height as f64 / 2.0,
        )
    }

    /// The canonical reference token for this element in the given generation.
    ///
    /// Always of the form `@g<generation>:e<ref_id>` so an agent carries the
    /// generation bound to the reference. `resolve_ref_with_generation` then
    /// detects a stale reference whose generation no longer matches the current
    /// page, even if the same `e<num>` was reused after a navigation.
    pub fn to_ref_token(&self, generation: u32) -> String {
        format!("@g{}:e{}", generation, self.ref_id)
    }

    /// Stamp the canonical `ref` token onto this element for `generation`.
    /// Called by the registry after assigning a ref id and generation.
    pub fn stamp_ref_token(&mut self, generation: u32) {
        self.ref_token = self.to_ref_token(generation);
    }

    /// Render as a compact one-line text line with generation prefix.
    /// e.g. `[@g12:e3] button "로그인"`.
    /// The generation prefix is the canonical reference format so agents
    /// always carry generation for stale detection.
    pub fn to_compact_line(&self, generation: u32) -> String {
        let state = match (self.checked, self.enabled, self.focused) {
            (Some(true), _, _) => " [checked]",
            (Some(false), _, _) => " [unchecked]",
            (None, false, _) => " [disabled]",
            (None, true, true) => " [focused]",
            _ => "",
        };
        let val = self
            .value
            .as_ref()
            .map(|v| format!(" = {v:?}"))
            .unwrap_or_default();
        format!(
            "[@g{}:e{}] {} {:?}{}{}",
            generation, self.ref_id, self.role, self.name, state, val
        )
    }

    /// Render with generation prefix: `[@g12:e3] button "Save"`.
    pub fn to_compact_line_with_generation(&self, generation: u32) -> String {
        let state = match (self.checked, self.enabled, self.focused) {
            (Some(true), _, _) => " [checked]",
            (Some(false), _, _) => " [unchecked]",
            (None, false, _) => " [disabled]",
            (None, true, true) => " [focused]",
            _ => "",
        };
        format!(
            "[@g{}:e{}] {} {:?}{}",
            generation, self.ref_id, self.role, self.name, state
        )
    }
}

/// A complete observation snapshot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Observation {
    /// Page-level metadata.
    pub page: super::PageMeta,
    /// Observed elements (sorted by reading order).
    pub elements: Vec<ElementRef>,
    /// The generation id of this observation.
    pub generation: u32,
    /// Navigation generation when this observation was taken.
    /// Used by `resolve_ref` to detect stale references after page navigation.
    #[serde(skip)]
    pub nav_generation: u32,
}

impl Observation {
    /// Render a compact human/agent-readable text view.
    pub fn to_compact(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "URL: {}\nTitle: {}\nViewport: {}x{}\nGeneration: {}\n",
            self.page.url,
            self.page.title,
            self.page.viewport.width,
            self.page.viewport.height,
            self.generation
        ));
        if self.elements.is_empty() {
            s.push_str("No interactive elements found.\n");
        } else {
            for el in &self.elements {
                s.push_str(&el.to_compact_line(self.generation));
                s.push('\n');
            }
        }
        s
    }

    /// Find an element by reference id.
    pub fn get(&self, id: RefId) -> Option<&ElementRef> {
        self.elements.iter().find(|e| e.ref_id == id)
    }
}

/// Registry mapping reference ids to elements, built per observe.
#[derive(Debug, Default)]
pub struct RefRegistry {
    inner: BTreeMap<RefId, ElementRef>,
    generation: u32,
}

impl RefRegistry {
    /// Create a fresh registry with a new generation id.
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
            generation: next_generation(),
        }
    }

    /// Insert an element, assigning it the next ref id in reading order and
    /// stamping the canonical `@g<gen>:e<num>` token using the registry's
    /// generation so the serialized element always carries its generation.
    pub fn insert(&mut self, mut el: ElementRef) {
        let id = (self.inner.len() as RefId) + 1;
        el.ref_id = id;
        el.stamp_ref_token(self.generation);
        self.inner.insert(id, el);
    }

    /// All elements sorted by ref id (reading order).
    pub fn into_sorted(self) -> Vec<ElementRef> {
        self.inner.into_values().collect()
    }

    /// Current generation.
    pub fn generation(&self) -> u32 {
        self.generation
    }
}

/// Parse a reference string into a numeric id.
///
/// Accepted formats:
/// - `@e3`, `e3`, `3` → ref id 3, generation None
/// - `@g12:e3` → ref id 3, generation 12 (explicit generation for stale detection)
pub fn parse_ref(s: &str) -> Result<RefId, String> {
    let s = s.trim();
    let s = s.strip_prefix('@').unwrap_or(s);
    // Check for generation prefix: g<gen>:e<num>
    if let Some(rest) = s.strip_prefix('g') {
        if let Some(colon) = rest.find(':') {
            let _gen_str = &rest[..colon];
            let ref_part = &rest[colon + 1..];
            let ref_part = ref_part.strip_prefix('e').unwrap_or(ref_part);
            return ref_part
                .parse::<RefId>()
                .map_err(|_| format!("invalid reference '{s}' (expected @g<gen>:eN)"));
        }
    }
    let s = s.strip_prefix('e').unwrap_or(s);
    s.parse::<RefId>()
        .map_err(|_| format!("invalid reference '{s}' (expected @eN or @g<gen>:eN)"))
}

/// Parse a reference string that may include a generation prefix.
/// Returns (ref_id, optional_generation).
pub fn parse_ref_with_generation(s: &str) -> Result<(RefId, Option<u32>), String> {
    let s = s.trim();
    let s = s.strip_prefix('@').unwrap_or(s);
    if let Some(rest) = s.strip_prefix('g') {
        if let Some(colon) = rest.find(':') {
            let gen_str = &rest[..colon];
            let ref_part = &rest[colon + 1..];
            let ref_part = ref_part.strip_prefix('e').unwrap_or(ref_part);
            let gen: u32 = gen_str
                .parse()
                .map_err(|_| format!("invalid generation in '{s}'"))?;
            let id: RefId = ref_part
                .parse()
                .map_err(|_| format!("invalid reference '{s}' (expected @g<gen>:eN)"))?;
            return Ok((id, Some(gen)));
        }
    }
    let id = parse_ref(s)?;
    Ok((id, None))
}

/// Generate a new observation generation id. Monotonic-ish per process.
fn next_generation() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static GEN: AtomicU32 = AtomicU32::new(1);
    GEN.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ref_variants() {
        assert_eq!(parse_ref("@e3").unwrap(), 3);
        assert_eq!(parse_ref("e7").unwrap(), 7);
        assert_eq!(parse_ref("12").unwrap(), 12);
        assert_eq!(parse_ref("@12").unwrap(), 12);
        assert_eq!(parse_ref("@g5:e3").unwrap(), 3);
        assert!(parse_ref("abc").is_err());
    }

    #[test]
    fn parse_ref_with_generation_variants() {
        let (id, gen) = parse_ref_with_generation("@g12:e3").unwrap();
        assert_eq!(id, 3);
        assert_eq!(gen, Some(12));

        let (id, gen) = parse_ref_with_generation("@e7").unwrap();
        assert_eq!(id, 7);
        assert_eq!(gen, None);

        let (id, gen) = parse_ref_with_generation("5").unwrap();
        assert_eq!(id, 5);
        assert_eq!(gen, None);
    }

    #[test]
    fn registry_assigns_sequential_ids() {
        let mut r = RefRegistry::new();
        r.insert(ElementRef {
            ref_id: 0,
            role: "button".into(),
            name: "Save".into(),
            tag_name: "button".into(),
            x: 10,
            y: 10,
            width: 40,
            height: 20,
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            value: None,
            selector_hint: String::new(),
            visual: false,
            ref_token: String::new(),
        });
        r.insert(ElementRef {
            ref_id: 0,
            role: "link".into(),
            name: "Home".into(),
            tag_name: "a".into(),
            x: 10,
            y: 40,
            width: 40,
            height: 20,
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            value: None,
            selector_hint: String::new(),
            visual: false,
            ref_token: String::new(),
        });
        let v = r.into_sorted();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].ref_id, 1);
        assert_eq!(v[1].ref_id, 2);
    }

    #[test]
    fn element_center() {
        let e = ElementRef {
            ref_id: 1,
            role: "button".into(),
            name: "x".into(),
            tag_name: "button".into(),
            x: 100,
            y: 200,
            width: 40,
            height: 60,
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            value: None,
            selector_hint: String::new(),
            visual: false,
            ref_token: String::new(),
        };
        let (cx, cy) = e.center();
        assert_eq!(cx, 120.0);
        assert_eq!(cy, 230.0);
    }
}
