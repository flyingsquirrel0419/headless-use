# Observe Coverage + Click Feedback — Design

Date: 2026-07-27
Status: Approved (design), pending implementation plan

## Problem

Field feedback from agent-driven use surfaced two gaps:

1. **Observe misses non-standard widgets.** `looksClickable()` in
   `src/observe/extract_elements.js` only checks the `onclick` attribute,
   `cursor: pointer`, and canvas/svg tags. Elements whose handlers are attached
   via `addEventListener` carry no DOM-visible signal, and event-delegation
   patterns (one listener on a container or `document`, inert child cells —
   e.g. a tic-tac-toe board) leave the actual click targets invisible to
   observe. Canvas widgets appear as a single element with no interior
   structure. The agent ends up hand-computing coordinates via
   `page.evaluate`.

2. **Clicks are fire-and-forget.** `Session::click` dispatches
   `Input.dispatchMouseEvent` and returns nothing about what happened. When a
   click has no visible effect, the agent cannot distinguish "click missed the
   target" from "application logic rejected it" from "timing issue", so it
   retries and re-screenshots, wasting tokens. The infrastructure to detect
   effects (MutationObserver + network tracker in `src/session/wait.rs`,
   `src/session/network_tracker.rs`) already exists but is not wired into the
   click result.

## Goals

- Observe detects elements with real (programmatically attached) click
  listeners, without being detectable by page scripts.
- Where enumeration is impossible in principle (delegation, canvas), observe
  says so explicitly instead of silently omitting — the agent learns that a
  gap exists and where it is.
- Every click returns a report: what was actually hit, and what observable
  effects followed, within a short bounded window. No opt-in flag; it is the
  default.

## Non-Goals

- Perfect enumeration of delegated targets or canvas hit regions (impossible
  in general; handled by the honest fallback instead).
- Page-script instrumentation (e.g. monkey-patching
  `EventTarget.addEventListener`). Rejected: detectable by the page,
  conflicting with this project's stealth posture.
- Changing the low-level input layer (`src/input/mouse.rs`). The report is a
  session-layer concern.

## Design

### A. Observe enhancement

#### A1. Listener detection via CDP `DOMDebugger.getEventListeners`

A new pass runs after the existing selector passes in
`ObserveBuilder::build()`:

1. Collect candidate elements not already captured: visible, within the
   viewport, non-trivial size, not matched by `INTERACTIVE_SELECTOR` /
   `VISUAL_SELECTOR`.
2. For each candidate (bounded, batched), resolve the node to a remote object
   and call `DOMDebugger.getEventListeners`.
3. Candidates with a `click`, `mousedown`, `pointerdown`, or `pointerup`
   listener are promoted to interactive elements with the same ref/role/bounds
   treatment as selector-matched elements.

CDP-side inspection is invisible to page JavaScript, preserving stealth.

Performance guards:
- Only elements intersecting the viewport are queried.
- Hard cap on the number of candidates per observation (default 150; excess
  is dropped and the observation notes truncation).
- Queries are batched over the CDP connection.

#### A2. Honest fallback: opaque interactive regions

Some interactive surfaces cannot be enumerated:

- **Delegation containers**: an element with a click-family listener and many
  (default ≥ 8) element children, none of which is itself independently
  interactive. The listener parent is real; which children respond is
  unknowable statically.
- **Canvas**: one element, interior hit regions invisible to the DOM.

These are emitted as observation entries flagged `opaque_interactive: true`,
carrying role (if any), bounds, and a short machine hint. The agent-facing
rendering of the observation includes wording equivalent to:

> This region receives clicks, but its interior targets cannot be enumerated.
> Choose coordinates from the screenshot.

The point is that the agent learns the gap exists, instead of concluding
"nothing here is clickable".

`document`/`body`-level delegation (listener on the root) is out of scope for
flagging — flagging the whole page as opaque is noise. Root-level listeners
are ignored by A2.

### B. Click result report

The click session operation (in `src/session/`, exposed through
`src/cli/rpc.rs`) returns a structured report. `src/input/mouse.rs` is
unchanged.

Response shape (extends the existing click RPC result):

```json
{
  "hit": {
    "element": "td.cell",
    "matched_target": true,
    "occluded_by": null
  },
  "effects": {
    "dom_mutations": 3,
    "network_requests": 1,
    "navigated": false,
    "focus_changed": false
  }
}
```

#### B1. Pre-click hit test

Immediately before dispatch, evaluate `document.elementFromPoint(x, y)`.

- `hit.element`: compact descriptor of the element that will receive the
  event (tag, id, salient classes; the observe ref if the element carries
  one).
- When the click was addressed to a ref: `matched_target` is true if the
  hit element is the target or an ancestor/descendant of it. Otherwise
  `matched_target: false` and `occluded_by` describes the intercepting
  element (typical case: overlay/modal/toast covering the target).

The hit test is advisory: the click is still dispatched even when occluded,
but the agent is told before wasting a retry cycle.

#### B2. Post-click observation window

After dispatch, observe for a bounded window (default 300 ms, configurable
per session):

- `dom_mutations`: mutation count from the already-installed
  MutationObserver (`src/session/wait.rs`).
- `network_requests`: requests started during the window
  (`src/session/network_tracker.rs`).
- `navigated`: navigation/URL change during the window (session nav
  generation counter).
- `focus_changed`: `document.activeElement` differs from before the click.

All-zero effects mean the response explicitly reports no observable effect —
the agent knows immediately, without a screenshot, that the click did
nothing.

Known limitation: a canvas application that reacts by repainting the canvas
only (no DOM/attribute change, no network) produces zero reported effects —
canvas paints are invisible to MutationObserver. For canvas surfaces the
agent still needs a screenshot to confirm a reaction; the `opaque_interactive`
flag on the canvas is the cue.

The window is a fixed short delay, not `wait_until_stable()`; callers that
want full stabilization continue to call that explicitly as today.

## Error Handling

- `getEventListeners` failures on individual nodes (detached, cross-frame)
  are skipped silently; the observation is still produced.
- Hit-test evaluation failure degrades to `hit: null` in the report; the
  click still dispatches.
- The observation window never blocks longer than its configured bound, even
  if the MutationObserver or network tracker misbehaves.

## Testing

Three fixture pages, each exercised end-to-end (observe output + click
report assertions):

1. **Delegated tic-tac-toe** — listener on the board container, cells are
   plain elements. Expect: board emitted as `opaque_interactive`; click on a
   cell coordinate reports `dom_mutations > 0`; click on an already-taken
   cell (game logic rejects) reports zero effects.
2. **Canvas game** — expect canvas flagged `opaque_interactive`; clicks
   report effects when the game reacts.
3. **Overlay occlusion** — target covered by an overlay. Expect
   `matched_target: false` with `occluded_by` naming the overlay, and zero
   effects.

Plus unit coverage for: candidate capping/truncation note, delegation
child-count threshold, report shape when hit test fails.
