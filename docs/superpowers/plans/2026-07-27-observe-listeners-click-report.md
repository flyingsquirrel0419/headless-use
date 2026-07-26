# Observe Listener Detection + Click Result Report Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Observe detects elements with programmatically-attached click listeners and honestly flags un-enumerable regions; every click returns a report of what was hit and what effects followed.

**Architecture:** A third extraction pass in `extract_elements.js` collects candidate elements; a new Rust module (`src/observe/listeners.rs`) queries CDP `DOMDebugger.getEventListeners` per candidate (invisible to page scripts) and promotes/flags them. Click reporting is a session-layer concern: `Session::click` gains a pre-click `elementFromPoint` hit test and a post-click bounded observation window reusing the existing MutationObserver (`wait.rs`) and `NetworkTracker`. `src/input/mouse.rs` is untouched.

**Tech Stack:** Rust (tokio), CDP (`Runtime.evaluate`, `DOMDebugger.getEventListeners`), in-page JS, existing integration-test harness (`tests/common`, `FixtureServer`, fixture HTML pages).

**Spec:** `docs/superpowers/specs/2026-07-27-observe-click-feedback-design.md`

## Global Constraints

- No page-script instrumentation (no monkey-patching `addEventListener`) — stealth posture.
- `src/input/mouse.rs` must not change.
- Listener pass: viewport-only candidates, hard cap 150 per observation, truncation surfaced on the Observation.
- Delegation container threshold: ≥ 8 element children, none independently interactive.
- Root-level delegation (`document`/`documentElement`/`body`) is never flagged.
- Click observation window default 300 ms, configurable per session; window of zero skips effect sampling.
- Click listener types that count: `click`, `mousedown`, `pointerdown`, `pointerup`.
- Per-node `getEventListeners` failures are skipped silently; hit-test failure degrades to `hit: null`; the click always dispatches.
- Element cap in `extract_elements.js` stays 200; `pushElement` behavior for existing passes must not regress.
- Commit messages: conventional commits, no Claude co-author trailer.
- Run integration tests with `cargo test --test <name>` (they launch real Chromium via `tests/common`).

---

### Task 1: `opaque_interactive` on ElementRef + `truncated` on Observation

**Files:**
- Modify: `src/observe/reference.rs`
- Test: unit tests inside `src/observe/reference.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `ElementRef.opaque_interactive: bool` (serde name `opaqueInteractive`, default false, skipped when false), `Observation.truncated: bool` (default false), compact-line rendering of the opaque hint. Later tasks deserialize `opaqueInteractive` from JS and set `truncated`.

- [ ] **Step 1: Write failing tests**

Append to the `tests` module in `src/observe/reference.rs` (reuse the existing style; note every existing `ElementRef { .. }` literal in tests must gain `opaque_interactive: false` once the field exists — do that in Step 3):

```rust
    #[test]
    fn opaque_element_renders_hint() {
        let e = ElementRef {
            ref_id: 4,
            role: "div".into(),
            name: "board".into(),
            tag_name: "div".into(),
            x: 0,
            y: 0,
            width: 300,
            height: 300,
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            value: None,
            selector_hint: "#board".into(),
            visual: true,
            opaque_interactive: true,
            ref_token: String::new(),
        };
        let line = e.to_compact_line(7);
        assert!(
            line.contains("[opaque: receives clicks; interior targets not enumerable — pick coordinates from the screenshot]"),
            "line was: {line}"
        );
    }

    #[test]
    fn opaque_deserializes_from_js_key() {
        let v = serde_json::json!({
            "role": "div", "name": "b", "tagName": "div",
            "x": 0, "y": 0, "width": 10, "height": 10,
            "visible": true, "enabled": true, "focused": false,
            "checked": null, "opaqueInteractive": true
        });
        let el: ElementRef = serde_json::from_value(v).unwrap();
        assert!(el.opaque_interactive);
    }

    #[test]
    fn truncated_observation_notes_it_in_compact() {
        let obs = Observation {
            page: crate::observe::PageMeta {
                url: "u".into(),
                title: "t".into(),
                viewport: crate::cdp::Viewport { width: 100, height: 100, device_scale_factor: 1.0 },
                scroll_x: 0,
                scroll_y: 0,
            },
            elements: vec![],
            generation: 1,
            nav_generation: 0,
            truncated: true,
        };
        assert!(obs.to_compact().contains("listener scan truncated"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p headless-use --lib observe::reference`
Expected: FAIL — `opaque_interactive` field does not exist (compile error).

- [ ] **Step 3: Implement**

In `src/observe/reference.rs`:

Add to `ElementRef` (after the `visual` field):

```rust
    /// Interactive surface whose interior targets cannot be enumerated
    /// (event-delegation container, canvas). The agent should pick
    /// coordinates from the screenshot instead of expecting child refs.
    #[serde(
        rename = "opaqueInteractive",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub opaque_interactive: bool,
```

In `to_compact_line` (and only there — `to_compact_line_with_generation` is a near-duplicate; extend it identically), append after the existing `format!`:

```rust
        let mut line = format!(
            "[@g{}:e{}] {} {:?}{}{}",
            generation, self.ref_id, self.role, self.name, state, val
        );
        if self.opaque_interactive {
            line.push_str(" [opaque: receives clicks; interior targets not enumerable — pick coordinates from the screenshot]");
        }
        line
```

Add to `Observation`:

```rust
    /// True when the listener-detection pass hit its candidate cap and some
    /// candidates were not scanned.
    #[serde(default)]
    pub truncated: bool,
```

In `Observation::to_compact`, after the element loop:

```rust
        if self.truncated {
            s.push_str("Note: listener scan truncated (candidate cap reached); some clickable elements may be missing.\n");
        }
```

Fix all compile errors: every `ElementRef { .. }` literal (tests in this file and anywhere else in the crate — grep `ref_token: String::new()`) gains `opaque_interactive: false`; every `Observation { .. }` literal gains `truncated: false`. `ObserveBuilder::build` in `src/observe/mod.rs` constructs an `Observation` — add `truncated: false` there for now (Task 3 sets it for real).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p headless-use --lib observe::reference` then `cargo build --all-targets`
Expected: PASS, clean build.

- [ ] **Step 5: Commit**

```bash
git add src/observe/reference.rs src/observe/mod.rs
git commit -m "feat(observe): opaque_interactive element flag and truncated observation note"
```

---

### Task 2: Candidate collection pass in `extract_elements.js` + new fixture

**Files:**
- Modify: `src/observe/extract_elements.js`
- Modify: `src/observe/mod.rs` (parse new return shape)
- Create: `tests/fixtures/delegated-tictactoe.html`
- Test: `tests/it_observe_listeners.rs` (new)

**Interfaces:**
- Consumes: `ElementRef.opaque_interactive` from Task 1.
- Produces: `extract_elements.js` now returns `{ elements: [...], candidates: [...], truncated: bool }` and stores candidate DOM nodes in `window.__hu_cand__` (array, same order as `candidates`). Each candidate object: `{ index, role, name, tagName, x, y, width, height, childCount, selectorHint }`. Canvas elements captured by the visual pass are emitted with `opaqueInteractive: true`. Task 3 consumes `candidates` + `window.__hu_cand__`.

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/delegated-tictactoe.html`:

```html
<!DOCTYPE html><html><head><meta charset="utf-8"><title>Delegated TicTacToe</title>
<style>
  #board { display: grid; grid-template-columns: repeat(3, 60px); width: 180px; }
  .cell { width: 60px; height: 60px; border: 1px solid #333; font-size: 40px; text-align: center; line-height: 60px; }
</style></head>
<body>
<h2>TicTacToe</h2>
<!-- Delegation: ONE listener on the board; cells are plain divs with no
     onclick attribute, no cursor:pointer, no role. Current observe cannot
     see them at all. -->
<div id="board">
  <div class="cell" data-i="0"></div><div class="cell" data-i="1"></div><div class="cell" data-i="2"></div>
  <div class="cell" data-i="3"></div><div class="cell" data-i="4"></div><div class="cell" data-i="5"></div>
  <div class="cell" data-i="6"></div><div class="cell" data-i="7"></div><div class="cell" data-i="8"></div>
</div>
<div id="status">next: X</div>
<!-- Direct addEventListener on a plain div: no onclick attr, no pointer
     cursor. Invisible to current observe; the listener pass must promote it. -->
<div id="refresh" style="width:40px;height:40px;border:1px solid #999">↻</div>
<!-- Plain inert div: must NOT be promoted. -->
<div id="inert" style="width:100px;height:30px">just text</div>
<!-- Canvas: opaque interactive surface. -->
<canvas id="game" width="120" height="80"></canvas>
<script>
  let turn = 'X';
  document.getElementById('board').addEventListener('click', (ev) => {
    const cell = ev.target.closest('.cell');
    if (!cell || cell.textContent) return; // taken cell: game logic rejects, no DOM change
    cell.textContent = turn;
    turn = turn === 'X' ? 'O' : 'X';
    document.getElementById('status').textContent = 'next: ' + turn;
  });
  document.getElementById('refresh').addEventListener('click', () => {
    document.getElementById('status').textContent = 'refreshed';
  });
</script>
</body></html>
```

- [ ] **Step 2: Write the failing test**

Create `tests/it_observe_listeners.rs`:

```rust
//! Integration tests: listener-detection observe pass + opaque flagging.

mod common;

async fn session() -> (
    common::TempProfile,
    headless_use::session::Session,
    common::FixtureServer,
) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(profile.launch_opts())
        .await
        .expect("session start");
    (profile, s, srv)
}

#[tokio::test]
async fn canvas_is_flagged_opaque() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let canvas = obs
        .elements
        .iter()
        .find(|e| e.tag_name == "canvas")
        .expect("canvas captured");
    assert!(canvas.opaque_interactive, "canvas must be opaque_interactive");
    s.shutdown().await;
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --test it_observe_listeners canvas_is_flagged_opaque`
Expected: FAIL — `opaque_interactive` is false (JS does not emit the flag yet).

- [ ] **Step 4: Implement the JS changes**

In `src/observe/extract_elements.js`:

(a) Track pushed element identities. In `pushElement`, on success add the element to a module-level `const pushedEls = new Set();` (declare next to `const out = [];`):

```js
    pushedEls.add(el);
```

(b) In the visual pass, when pushing a canvas, mark it opaque. Change the `pushElement` call site in the visual loop:

```js
    const isOpaqueSurface = el.tagName.toLowerCase() === 'canvas';
    if (pushElement(el, { role, name: visualName(el), visual: true, opaque: isOpaqueSurface })) {
      if (hint) seen.add(hint);
    }
```

and in `pushElement`'s output object add:

```js
      opaqueInteractive: !!opts.opaque,
```

(c) Third pass after the visual pass, before `return out;`:

```js
  // Third pass: listener candidates. Plain elements that MIGHT have
  // programmatically-attached click listeners. We cannot see listeners from
  // page JS; the Rust side asks CDP DOMDebugger.getEventListeners per
  // candidate. We only nominate viewport-visible, reasonably-sized elements
  // not already captured, capped to keep the CDP round-trips bounded.
  const CANDIDATE_CAP = 150;
  const vw = window.innerWidth, vh = window.innerHeight;
  const cands = [];
  window.__hu_cand__ = [];
  let truncated = false;
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (cands.length >= CANDIDATE_CAP) { truncated = true; break; }
    if (pushedEls.has(el)) continue;
    if (el === document.documentElement || el === document.body) continue;
    const tag = el.tagName.toLowerCase();
    if (tag === 'script' || tag === 'style' || tag === 'link' || tag === 'meta') continue;
    if (el.matches(INTERACTIVE_SELECTOR)) continue; // captured or hidden duplicate
    if (el.closest(INTERACTIVE_SELECTOR)) continue; // inside a captured element
    if (isHidden(el)) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) continue;
    if (r.right < 0 || r.bottom < 0 || r.left > vw || r.top > vh) continue; // outside viewport
    window.__hu_cand__.push(el);
    cands.push({
      index: cands.length,
      role: el.getAttribute('role') || tag,
      name: visualName(el),
      tagName: tag,
      x: Math.round(r.left),
      y: Math.round(r.top),
      width: Math.round(r.width),
      height: Math.round(r.height),
      childCount: el.childElementCount,
      selectorHint: el.id ? '#' + CSS.escape(el.id) : '',
    });
  }
  return { elements: out, candidates: cands, truncated };
```

(Replace the existing final `return out;` with the block above. Note `window.__hu_cand__` is intentionally rebuilt on every observe; Task 3 clears it after the CDP pass.)

- [ ] **Step 5: Update `src/observe/mod.rs` parsing**

In `ObserveBuilder::build`, replace the `raw_elements` parsing block:

```rust
        let raw = result
            .value()
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));

        let elements: Vec<ElementRef> = raw
            .get("elements")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| serde_json::from_value::<ElementRef>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let js_truncated = raw
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
```

Set `truncated: js_truncated` in the returned `Observation` (Task 3 will OR in the listener-pass result). The `candidates` array is ignored in this task.

- [ ] **Step 6: Run tests**

Run: `cargo test --test it_observe_listeners canvas_is_flagged_opaque` and `cargo test --test it_core`
Expected: both PASS (existing observe behavior unchanged for standard elements).

- [ ] **Step 7: Commit**

```bash
git add src/observe/extract_elements.js src/observe/mod.rs tests/fixtures/delegated-tictactoe.html tests/it_observe_listeners.rs
git commit -m "feat(observe): candidate collection pass and opaque canvas flagging"
```

---

### Task 3: CDP listener detection pass (`src/observe/listeners.rs`)

**Files:**
- Create: `src/observe/listeners.rs`
- Modify: `src/observe/mod.rs` (declare module, wire into `build()`)
- Test: `tests/it_observe_listeners.rs` (extend)

**Interfaces:**
- Consumes: `candidates` JSON + `window.__hu_cand__` from Task 2; `ElementRef` from Task 1; `Page::call` / `Page::evaluate_sync`.
- Produces: `pub(crate) async fn detect(page: &Page, candidates: &[Candidate]) -> (Vec<ElementRef>, bool)` — promoted elements (with `visual: true`, and `opaque_interactive: true` for delegation containers) plus a `truncated`-style error flag is NOT produced here (cap truncation came from JS); the bool is `any_scan_failed` and is only logged. `ObserveBuilder::build` appends the promoted elements before registry insertion.

- [ ] **Step 1: Write failing tests**

Append to `tests/it_observe_listeners.rs`:

```rust
#[tokio::test]
async fn direct_listener_div_is_promoted() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let refresh = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#refresh")
        .expect("refresh div promoted via getEventListeners");
    assert!(refresh.visual, "listener-promoted elements are heuristic (visual)");
    assert!(!refresh.opaque_interactive, "few children: not opaque");
    s.shutdown().await;
}

#[tokio::test]
async fn delegation_container_is_opaque() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let board = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#board")
        .expect("board container found");
    assert!(board.opaque_interactive, "9 inert children + listener = opaque");
    s.shutdown().await;
}

#[tokio::test]
async fn inert_div_is_not_promoted() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    assert!(
        !obs.elements.iter().any(|e| e.selector_hint == "#inert"),
        "no listener: must not be promoted"
    );
    s.shutdown().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test it_observe_listeners`
Expected: `direct_listener_div_is_promoted` and `delegation_container_is_opaque` FAIL; the other two PASS.

- [ ] **Step 3: Implement `src/observe/listeners.rs`**

```rust
//! Listener-detection pass: promote candidate elements that carry real
//! programmatically-attached click listeners.
//!
//! ## Why CDP `DOMDebugger.getEventListeners` (not addEventListener patching)
//! Patching `EventTarget.addEventListener` from an injected script would catch
//! the same listeners but is observable by page JavaScript, which conflicts
//! with this project's stealth posture. The CDP query happens entirely on the
//! protocol side; the page cannot detect it.
//!
//! ## Delegation honesty
//! A container with a click listener and many inert element children is a
//! delegation pattern (e.g. a game board). Which children respond cannot be
//! determined statically, so the container is flagged `opaque_interactive`
//! instead of pretending the children are not clickable.

use serde_json::{json, Value};

use crate::browser::Page;
use crate::observe::reference::ElementRef;

/// Listener types that make an element click-interactive.
const CLICK_LISTENER_TYPES: [&str; 4] = ["click", "mousedown", "pointerdown", "pointerup"];

/// A container with at least this many element children (and a click
/// listener) is treated as a delegation surface and flagged opaque.
const DELEGATION_MIN_CHILDREN: u32 = 8;

/// One candidate nominated by extract_elements.js's third pass.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Candidate {
    pub index: u32,
    pub role: String,
    pub name: String,
    #[serde(rename = "tagName")]
    pub tag_name: String,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    #[serde(rename = "childCount")]
    pub child_count: u32,
    #[serde(rename = "selectorHint", default)]
    pub selector_hint: String,
}

/// Query CDP for listeners on each candidate; return the promoted elements.
///
/// Per-candidate failures (node detached, evaluate error) are skipped
/// silently per spec — a partial observation beats none.
pub(crate) async fn detect(page: &Page, candidates: &[Candidate]) -> Vec<ElementRef> {
    let mut promoted = Vec::new();
    for cand in candidates {
        if let Some(has_listener) = candidate_has_click_listener(page, cand.index).await {
            if has_listener {
                promoted.push(to_element(cand));
            }
        }
    }
    // Release the candidate handles and the objectGroup in one shot.
    let _ = page
        .evaluate_sync("window.__hu_cand__ = []; true")
        .await;
    let _ = page
        .call::<Value>(
            "Runtime.releaseObjectGroup",
            Some(json!({ "objectGroup": "hu-listeners" })),
            std::time::Duration::from_secs(5),
        )
        .await;
    promoted
}

/// Resolve `window.__hu_cand__[index]` to an objectId and ask DOMDebugger for
/// its listeners. `None` = scan failed for this node; `Some(false)` = scanned,
/// no click-family listener.
async fn candidate_has_click_listener(page: &Page, index: u32) -> Option<bool> {
    let eval: Value = page
        .call(
            "Runtime.evaluate",
            Some(json!({
                "expression": format!("window.__hu_cand__ && window.__hu_cand__[{index}]"),
                "returnByValue": false,
                "objectGroup": "hu-listeners",
            })),
            std::time::Duration::from_secs(5),
        )
        .await
        .ok()?;
    let object_id = eval
        .get("result")
        .and_then(|r| r.get("objectId"))
        .and_then(|o| o.as_str())?
        .to_string();
    let listeners: Value = page
        .call(
            "DOMDebugger.getEventListeners",
            Some(json!({ "objectId": object_id })),
            std::time::Duration::from_secs(5),
        )
        .await
        .ok()?;
    let has = listeners
        .get("listeners")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter().any(|l| {
                l.get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| CLICK_LISTENER_TYPES.contains(&t))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    Some(has)
}

fn to_element(cand: &Candidate) -> ElementRef {
    let opaque = cand.child_count >= DELEGATION_MIN_CHILDREN;
    ElementRef {
        ref_id: 0,
        role: cand.role.clone(),
        name: cand.name.clone(),
        tag_name: cand.tag_name.clone(),
        x: cand.x,
        y: cand.y,
        width: cand.width,
        height: cand.height,
        visible: true,
        enabled: true,
        focused: false,
        checked: None,
        value: None,
        selector_hint: cand.selector_hint.clone(),
        visual: true,
        opaque_interactive: opaque,
        ref_token: String::new(),
    }
}
```

Note: `Page::call` deserializes into any `DeserializeOwned` type — use `Value` here. Check `Page::call`'s exact return handling in `src/browser/page.rs:105` when wiring; if it strips the `result` wrapper differently, adjust the `eval.get("result")` path accordingly (verify by running the Task 3 tests).

- [ ] **Step 4: Wire into `ObserveBuilder::build`** (`src/observe/mod.rs`)

Declare the module: `pub(crate) mod listeners;` next to the other `pub mod` lines.

In `build()`, after parsing `elements` and `js_truncated`:

```rust
        let candidates: Vec<listeners::Candidate> = raw
            .get("candidates")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let promoted = listeners::detect(self.page, &candidates).await;

        let mut registry = RefRegistry::new();
        for el in elements.iter().chain(promoted.iter()) {
            registry.insert(el.clone());
        }
```

(Replace the existing `for el in &elements` loop.) Keep `truncated: js_truncated` on the Observation.

Also update `observe_with_mode` behavior check in `src/session/mod.rs`: `interactive-only` retains `!e.visual` — promoted/opaque elements have `visual: true`, so they are correctly excluded there. No code change; confirm the existing `it_p1_features.rs`/`it_core.rs` observe-mode tests still pass.

- [ ] **Step 5: Run tests**

Run: `cargo test --test it_observe_listeners` then `cargo test --test it_core`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add src/observe/listeners.rs src/observe/mod.rs tests/it_observe_listeners.rs
git commit -m "feat(observe): detect click listeners via CDP DOMDebugger and flag delegation containers opaque"
```

---

### Task 4: Mutation counter + network started counter

**Files:**
- Modify: `src/session/wait.rs` (observer counts mutations; `install_mutation_observer` becomes `pub(crate)`)
- Modify: `src/session/network_tracker.rs` (`started_count()`)
- Test: unit tests in `src/session/network_tracker.rs`; existing wait integration tests as regression

**Interfaces:**
- Produces: `pub(crate) async fn install_mutation_observer(page: &Page) -> Result<(), BrowserError>` (same fn, now pub(crate)); in-page `window.__hu_mut_count__` cumulative mutation count; `NetworkTracker::started_count(&self) -> u64` cumulative started-request count. Task 5 samples both before/after a click.

- [ ] **Step 1: Write failing test**

Append to the `tests` module in `src/session/network_tracker.rs`:

```rust
    #[tokio::test]
    async fn started_count_is_cumulative() {
        let tracker = NetworkTracker::new();
        assert_eq!(tracker.started_count().await, 0);
        for i in 0..3 {
            let params = json!({
                "requestId": format!("s{i}"),
                "request": { "method": "GET", "url": "https://x/y" }
            });
            let mut g = tracker.inner.lock().await;
            let id = format!("s{i}");
            g.record_started(TrackedRequest::from_started(&params));
            g.mark_finished(&id, Finisher::Completed);
        }
        assert_eq!(
            tracker.started_count().await,
            3,
            "finished requests still count as started"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p headless-use --lib session::network_tracker`
Expected: FAIL — no method `started_count` (compile error).

- [ ] **Step 3: Implement**

`src/session/network_tracker.rs`:
- Add field to `Inner`: `started_total: u64,` (init `0` in `NetworkTracker::new`).
- In `Inner::record_started`, first line: `self.started_total += 1;`
- Add method on `NetworkTracker`:

```rust
    /// Cumulative count of requests started since the tracker was created.
    /// Sampled before/after a click to report network effects.
    pub async fn started_count(&self) -> u64 {
        self.inner.lock().await.started_total
    }
```

`src/session/wait.rs`:
- Change `async fn install_mutation_observer` to `pub(crate) async fn install_mutation_observer`.
- In its JS `expr`, extend the observer to count (replace the observer construction):

```js
  window.__hu_mut_count__ = window.__hu_mut_count__ || 0;
  const obs = new MutationObserver((muts) => {
    window.__hu_last_mut__ = Date.now();
    window.__hu_mut_count__ += muts.length;
  });
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p headless-use --lib session::network_tracker` and `cargo test --test it_core`
Expected: PASS (wait behavior unchanged — the counter is additive).

- [ ] **Step 5: Commit**

```bash
git add src/session/wait.rs src/session/network_tracker.rs
git commit -m "feat(session): cumulative mutation and network-start counters for click effect sampling"
```

---

### Task 5: Click report (`src/session/click_report.rs` + `Session::click`)

**Files:**
- Create: `src/session/click_report.rs`
- Modify: `src/session/mod.rs` (`Session::click` returns `ClickReport`; new `click_observe_window` field + builder; declare module)
- Modify: `src/trace/replay.rs:307` (discard the report)
- Test: `tests/it_click_report.rs` (new)

**Interfaces:**
- Consumes: `install_mutation_observer` + `__hu_mut_count__` + `started_count()` from Task 4; `nav_generation_value()`; fixtures `delegated-tictactoe.html` (Task 2) and `overlay-blocking.html` (existing).
- Produces:

```rust
pub struct HitInfo { pub element: Option<String>, pub matched_target: Option<bool>, pub occluded_by: Option<String> }
pub struct ClickEffects { pub dom_mutations: u64, pub network_requests: u64, pub navigated: bool, pub focus_changed: bool }
pub struct ClickReport { pub hit: Option<HitInfo>, pub effects: Option<ClickEffects> }
```

`Session::click(...) -> Result<ClickReport, BrowserError>`; `Session::with_click_observe_window(Duration) -> Self`. Task 6 serializes `ClickReport` into the RPC response.

- [ ] **Step 1: Write failing tests**

Create `tests/it_click_report.rs`:

```rust
//! Integration tests: click hit test + effect observation window.

mod common;

use std::time::Duration;

use headless_use::input::{Modifiers, MouseButton, Point};
use headless_use::session::ClickTarget;

async fn session() -> (
    common::TempProfile,
    headless_use::session::Session,
    common::FixtureServer,
) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(profile.launch_opts())
        .await
        .expect("session start");
    (profile, s, srv)
}

async fn click_at(
    s: &headless_use::session::Session,
    x: f64,
    y: f64,
) -> headless_use::session::click_report::ClickReport {
    s.click(
        ClickTarget::Point(Point::new(x, y)),
        MouseButton::Left,
        1,
        Modifiers::NONE,
        Duration::ZERO,
    )
    .await
    .unwrap()
}

/// Center of the fixture element with the given id, via observe.
async fn center_of(s: &headless_use::session::Session, hint: &str) -> (f64, f64) {
    let obs = s.observe().await.unwrap();
    let el = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == hint)
        .unwrap_or_else(|| panic!("{hint} not observed"));
    el.center()
}

#[tokio::test]
async fn effective_click_reports_mutations() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let (bx, by) = center_of(&s, "#board").await;
    // Top-left cell: board center minus one cell (cells are 60px).
    let report = click_at(&s, bx - 60.0, by - 60.0).await;
    let effects = report.effects.expect("default window is on");
    assert!(
        effects.dom_mutations > 0,
        "placing an X mutates the DOM: {effects:?}"
    );
    assert!(!effects.navigated);
    s.shutdown().await;
}

#[tokio::test]
async fn rejected_click_reports_zero_effects() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let (bx, by) = center_of(&s, "#board").await;
    let cell = (bx - 60.0, by - 60.0);
    let first = click_at(&s, cell.0, cell.1).await;
    assert!(first.effects.unwrap().dom_mutations > 0);
    // Same (now taken) cell: game logic ignores the click entirely.
    let second = click_at(&s, cell.0, cell.1).await;
    let effects = second.effects.expect("default window is on");
    assert_eq!(
        effects.dom_mutations, 0,
        "taken cell: no DOM change, agent must see zero effects"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn occluded_target_is_reported() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("overlay-blocking.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let button = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#underneath")
        .expect("underneath button observed");
    let target = headless_use::session::Session::click_target_from_ref(&button.ref_token).unwrap();
    let report = s
        .click(target, MouseButton::Left, 1, Modifiers::NONE, Duration::ZERO)
        .await
        .unwrap();
    let hit = report.hit.expect("hit test ran");
    assert_eq!(hit.matched_target, Some(false), "overlay covers the button");
    let occluder = hit.occluded_by.expect("occluder reported");
    assert!(occluder.contains("overlay"), "occluder was: {occluder}");
    s.shutdown().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test it_click_report`
Expected: FAIL — `click_report` module and `ClickReport` return type do not exist (compile error).

- [ ] **Step 3: Implement `src/session/click_report.rs`**

```rust
//! Click result reporting: pre-click hit test + post-click effect window.
//!
//! ## Why
//! A dispatched click that produces no visible change is ambiguous to the
//! agent: did it miss, did application logic reject it, or was it timing?
//! Reporting what was actually hit and what observably followed removes the
//! retry-and-screenshot loop. See the design spec
//! (docs/superpowers/specs/2026-07-27-observe-click-feedback-design.md).

use std::time::Duration;

use serde_json::Value;

use crate::browser::{BrowserError, Page};
use crate::session::network_tracker::NetworkTracker;

/// What `document.elementFromPoint` said the click would land on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HitInfo {
    /// Compact descriptor of the element at the click point
    /// (e.g. `td#c3` / `div.overlay`), or None if the point hit nothing.
    pub element: Option<String>,
    /// When the click was addressed to an observe ref with a selector hint:
    /// whether the hit element is that element (or its ancestor/descendant).
    /// None when the click was a raw coordinate or the target had no hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_target: Option<bool>,
    /// Descriptor of the intercepting element when `matched_target` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occluded_by: Option<String>,
}

/// What observably happened within the post-click window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClickEffects {
    /// DOM mutations recorded by the session MutationObserver.
    pub dom_mutations: u64,
    /// Network requests started during the window.
    pub network_requests: u64,
    /// Whether a main-frame navigation occurred.
    pub navigated: bool,
    /// Whether `document.activeElement` changed.
    pub focus_changed: bool,
}

impl ClickEffects {
    /// True when nothing observable happened — the agent's "dead click" signal.
    pub fn is_empty(&self) -> bool {
        self.dom_mutations == 0
            && self.network_requests == 0
            && !self.navigated
            && !self.focus_changed
    }
}

/// The full report returned by `Session::click`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClickReport {
    /// Pre-click hit test result. None if the evaluation failed.
    pub hit: Option<HitInfo>,
    /// Post-click effects. None when the observe window is zero.
    pub effects: Option<ClickEffects>,
}

/// Run the pre-click hit test. Failure degrades to `None` — the click must
/// dispatch regardless.
pub(crate) async fn hit_test(page: &Page, x: f64, y: f64, target_hint: Option<&str>) -> Option<HitInfo> {
    let hint_js = match target_hint {
        Some(h) => format!("{:?}", h), // JSON-escapes the selector string
        None => "null".to_string(),
    };
    let expr = format!(
        r#"(() => {{
  const hit = document.elementFromPoint({x}, {y});
  const desc = (el) => {{
    if (!el) return null;
    let s = el.tagName.toLowerCase();
    if (el.id) s += '#' + el.id;
    else if (el.classList && el.classList.length) s += '.' + Array.from(el.classList).slice(0, 2).join('.');
    return s;
  }};
  const hint = {hint_js};
  let matched = null, occluded = null;
  if (hit && hint) {{
    const target = document.querySelector(hint);
    if (target) {{
      matched = (hit === target) || target.contains(hit) || hit.contains(target);
      if (!matched) occluded = desc(hit);
    }}
  }}
  return JSON.stringify({{ element: desc(hit), matched, occluded }});
}})()"#
    );
    let v = page.evaluate_sync(&expr).await.ok()?;
    let s = v.value()?.as_str()?.to_string();
    let obj: Value = serde_json::from_str(&s).ok()?;
    Some(HitInfo {
        element: obj.get("element").and_then(|e| e.as_str()).map(String::from),
        matched_target: obj.get("matched").and_then(|m| m.as_bool()),
        occluded_by: obj.get("occluded").and_then(|o| o.as_str()).map(String::from),
    })
}

/// Snapshot taken immediately before dispatching the click.
pub(crate) struct EffectsBaseline {
    mutations: u64,
    started: u64,
    nav_generation: u32,
    focus: String,
}

const BASELINE_EXPR: &str = r#"(() => {
  const el = document.activeElement;
  const focus = el ? el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') : '';
  return JSON.stringify({ mut: window.__hu_mut_count__ || 0, focus });
})()"#;

impl EffectsBaseline {
    /// Capture counters before the click. The MutationObserver must already be
    /// installed (Session::click does that).
    pub(crate) async fn capture(
        page: &Page,
        tracker: &NetworkTracker,
        nav_generation: u32,
    ) -> Result<Self, BrowserError> {
        let v = page.evaluate_sync(BASELINE_EXPR).await?;
        let obj: Value = v
            .value()
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        Ok(Self {
            mutations: obj.get("mut").and_then(|m| m.as_u64()).unwrap_or(0),
            started: tracker.started_count().await,
            nav_generation,
            focus: obj
                .get("focus")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Wait out the observation window, then compute deltas.
    ///
    /// If a navigation happened, in-page counters belong to the NEW page
    /// (`__hu_mut_count__` restarts at 0), so JS-derived deltas use
    /// saturating arithmetic and the report leans on `navigated: true`.
    pub(crate) async fn finish(
        self,
        page: &Page,
        tracker: &NetworkTracker,
        nav_generation_now: u32,
        window: Duration,
    ) -> ClickEffects {
        tokio::time::sleep(window).await;
        let obj: Value = page
            .evaluate_sync(BASELINE_EXPR)
            .await
            .ok()
            .and_then(|v| v.value().and_then(|v| v.as_str()).map(String::from))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut_now = obj.get("mut").and_then(|m| m.as_u64()).unwrap_or(0);
        let focus_now = obj
            .get("focus")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .to_string();
        let navigated = nav_generation_now != self.nav_generation;
        ClickEffects {
            dom_mutations: mut_now.saturating_sub(self.mutations),
            network_requests: tracker.started_count().await.saturating_sub(self.started),
            navigated,
            // A navigation resets activeElement to <body>; that is a
            // navigation signal, not a meaningful focus change.
            focus_changed: !navigated && focus_now != self.focus,
        }
    }
}
```

- [ ] **Step 4: Rework `Session::click` in `src/session/mod.rs`**

Declare the module (`pub mod click_report;`) and re-export: `pub use click_report::{ClickEffects, ClickReport, HitInfo};`

Add the session field (after `cursor_motion`):

```rust
    /// Post-click effect observation window. Zero disables effect sampling.
    click_observe_window: Duration,
```

Init in `start()`: `click_observe_window: Duration::from_millis(300),` and add the builder:

```rust
    /// Set the post-click effect observation window (default 300 ms).
    /// `Duration::ZERO` disables effect sampling; the hit test still runs.
    pub fn with_click_observe_window(mut self, window: Duration) -> Self {
        self.click_observe_window = window;
        self
    }
```

Replace `Session::click`:

```rust
    /// Click a reference or coordinate, returning what was hit and what
    /// observably followed. The click always dispatches, even when the hit
    /// test reports occlusion — the report is advisory.
    pub async fn click(
        &self,
        target: ClickTarget,
        button: MouseButton,
        count: u32,
        modifiers: crate::input::Modifiers,
        hold: Duration,
    ) -> Result<click_report::ClickReport, BrowserError> {
        // The target's selector hint (for matched_target), before resolution
        // consumes the target.
        let target_hint: Option<String> = match &target {
            ClickTarget::Ref { id, .. } => self
                .last_observation()
                .await
                .and_then(|obs| obs.get(*id).map(|el| el.selector_hint.clone()))
                .filter(|h| !h.is_empty()),
            ClickTarget::Point(_) => None,
        };
        let (x, y) = self.resolve_click_point(target).await?;
        self.record_action(
            "mouse.click",
            json!({ "x": x, "y": y, "button": button.as_cdp(), "count": count }),
        )
        .await;

        // Effects need the MutationObserver; install is idempotent. Baseline
        // and hit test both run before any mouse movement.
        let sample_effects = !self.click_observe_window.is_zero();
        let baseline = if sample_effects {
            wait::install_mutation_observer(&self.page).await?;
            Some(
                click_report::EffectsBaseline::capture(
                    &self.page,
                    &self.network_tracker,
                    self.nav_generation_value(),
                )
                .await?,
            )
        } else {
            None
        };
        let hit = click_report::hit_test(&self.page, x, y, target_hint.as_deref()).await;

        self.travel_to((x, y).into(), modifiers).await?;
        self.mouse()
            .click((x, y).into(), button, count, modifiers, hold)
            .await?;

        let effects = match baseline {
            Some(b) => Some(
                b.finish(
                    &self.page,
                    &self.network_tracker,
                    self.nav_generation_value(),
                    self.click_observe_window,
                )
                .await,
            ),
            None => None,
        };
        Ok(click_report::ClickReport { hit, effects })
    }
```

Correction to the module code above: `nav_generation_value()` must be read **after** the window sleep (to catch navigations completing during it), but an argument is evaluated before the call. So the sleep lives in `Session::click`, and the method is named `finish_now` with no `window` parameter and no internal `tokio::time::sleep` — delete that line and the parameter from the `finish` implementation shown above and rename it:

```rust
        let effects = match baseline {
            Some(b) => {
                tokio::time::sleep(self.click_observe_window).await;
                Some(
                    b.finish_now(
                        &self.page,
                        &self.network_tracker,
                        self.nav_generation_value(),
                    )
                    .await,
                )
            }
            None => None,
        };
```

and in `click_report.rs` name the method `finish_now` WITHOUT the internal sleep (drop the `window` parameter and the `tokio::time::sleep` line from the implementation above). The sleep lives in `Session::click` so the fresh `nav_generation_value()` is read after it.

- [ ] **Step 5: Fix the other caller**

`src/trace/replay.rs:307` — the `.click(...).await?` result is now a `ClickReport`; the replayer does not use it. Ensure the statement discards it without a warning (`let _report = ... .await?;` or leave as an expression statement ending in `?;`).

- [ ] **Step 6: Run tests**

Run: `cargo test --test it_click_report` then `cargo test --test it_core --test it_input --test it_mouse_state`
Expected: all PASS. If `effective_click_reports_mutations` is flaky because the game's DOM update races the 300 ms window, the mutation arrives within milliseconds of the click — investigate rather than widen the window (systematic-debugging).

- [ ] **Step 7: Commit**

```bash
git add src/session/click_report.rs src/session/mod.rs src/trace/replay.rs tests/it_click_report.rs
git commit -m "feat(session): click returns hit test and post-click effect report"
```

---

### Task 6: RPC response + CHANGELOG

**Files:**
- Modify: `src/cli/rpc.rs` (click handler)
- Modify: `CHANGELOG.md`
- Test: `tests/it_rpc.rs` (extend)

**Interfaces:**
- Consumes: `ClickReport` (serde-Serialize) from Task 5.
- Produces: RPC `click` result `{ "success": true, "hit": ..., "effects": ... }`.

- [ ] **Step 1: Write failing test**

In `tests/it_rpc.rs`, the existing `serve_rpc_observe_click_type` test sends a `click` and asserts on the response. Extend it (or add a sibling assertion where the click response is received): after the click response arrives, assert the report fields are present:

```rust
    // Click response now carries the hit/effects report.
    let click_resp = recv(&mut reader, Duration::from_secs(20)).expect("click response");
    let result = click_resp.get("result").expect("result");
    assert_eq!(result.get("success"), Some(&json!(true)));
    assert!(result.get("effects").is_some(), "effects report missing: {result}");
    assert!(
        result.get("effects").unwrap().get("dom_mutations").is_some(),
        "effects.dom_mutations missing: {result}"
    );
```

(Adapt variable names to the test's actual reader/response flow — follow how the test already receives the click response.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test it_rpc serve_rpc_observe_click_type`
Expected: FAIL — response is `{ "success": true }` only.

- [ ] **Step 3: Implement**

`src/cli/rpc.rs` click arm:

```rust
        "click" => {
            let target = resolve_target(params)?;
            let button = parse_button(params)?;
            let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let mods = parse_modifiers_param(params);
            let hold_ms = params.get("hold").and_then(|v| v.as_u64()).unwrap_or(0);
            let report = session
                .click(target, button, count, mods, Duration::from_millis(hold_ms))
                .await?;
            Ok(json!({ "success": true, "hit": report.hit, "effects": report.effects }))
        }
```

`CHANGELOG.md`: add an entry under the unreleased/top section:

```markdown
- observe: detects programmatically-attached click listeners via CDP
  `DOMDebugger.getEventListeners`; delegation containers and canvas are
  flagged `opaqueInteractive` with an explicit "pick coordinates from the
  screenshot" hint instead of being silently omitted.
- click: every click now returns a report — pre-click hit test
  (`hit.element`, `matched_target`, `occluded_by`) and post-click effects
  within a 300 ms window (`dom_mutations`, `network_requests`, `navigated`,
  `focus_changed`). All-zero effects = dead click, no screenshot needed.
```

(Match the file's existing heading/format conventions.)

- [ ] **Step 4: Run tests**

Run: `cargo test --test it_rpc` then the full suite `cargo test`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli/rpc.rs CHANGELOG.md tests/it_rpc.rs
git commit -m "feat(rpc): expose click hit/effects report in click response"
```

---

## Final verification

- `cargo test` (full suite) green.
- `cargo clippy --all-targets` clean (match repo's existing lint bar).
- Manual sanity: `delegated-tictactoe.html` observe output shows `#board` with the opaque hint, `#refresh` promoted, `#inert` absent; a click on a taken cell reports zero effects.
