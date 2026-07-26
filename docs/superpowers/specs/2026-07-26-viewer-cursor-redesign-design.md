# Live viewer: cursor redesign and natural motion

Date: 2026-07-26
Status: implemented

## Problem

Three things made the live viewer hard to watch.

1. **The cursor competed with the page.** A 48px cyan arrow with a blurred glow
   halo sat on top of the content under test. On small controls it covered the
   thing being clicked, and it read as decoration rather than as a pointer.
2. **The cursor teleported.** `Session::click` called `Mouse::move_to`, a single
   jump. `move_smooth` existed but was reachable only through the `mouse.move`
   JSON-RPC method with an explicit `duration`. So in the stream the cursor
   vanished from one place and appeared in another, with no way to follow what
   the agent was doing. Hover menus that require actual movement also never
   opened.
3. **The viewer page wasted the window.** A header, a full-width image and a
   paragraph of explanation stacked down the page, so a 1280x720 stream
   overflowed the fold on a laptop.

## Decisions

### Cursor asset

Solid white fill, `#111` outline at 1.5px, `drop-shadow(0 2px 6px rgba(0,0,0,.45))`,
28px. White reads on dark pages, the dark outline reads on light ones, and the
shadow separates it from both — no glow needed, which also means no blur filter
repainting on every move. The drawn tip is offset so it lands exactly on the
event coordinate.

Click feedback is one expanding ripple (scale .2 → 1.6, opacity 1 → 0, 300ms).
The old persistent cyan ring looked like a UI element belonging to the page.
On press the arrow nudges instead of scaling up, so it does not hide more of
the control at the moment you want to see it.

Rejected: keeping the neon palette. It is a brand choice that costs legibility
on the only screen where it matters.

### Motion

```rust
pub enum CursorMotion {
    Instant,
    Smooth { duration: Duration, steps: u32 },  // default 220ms / 24 steps
}
```

Stored as a plain `Session` field — chosen once at construction, never mutated,
so there is nothing to lock. (`with_policy` previously used
`Mutex::blocking_lock` for the same shape of thing and panicked inside a tokio
runtime; a plain field cannot repeat that.)

`Session::click` and `Session::hover` route through `travel_to`, which either
jumps or eases along the existing smoothstep-plus-bow path in
`Mouse::move_smooth`.

`view` defaults to `Smooth`, `serve` to `Instant`, and `run`/`mcp` are fixed at
`Instant` with no flag — they are one-shot and agent-only, so the flag would be
surface without a use. `--cursor-motion smooth|instant` overrides the two that
have it.

220ms over 24 steps is ~9.2ms per step, just above the 8ms floor in
`move_smooth` below which Chrome's input queue stutters. A unit test asserts
this stays true.

Rejected: animating only in the overlay (zero agent cost, but the page never
receives the intermediate events, so the visual would not match the real input
and recorded traces would disagree with what was shown). Also rejected:
smoothing everywhere by default, which taxes every CI click ~220ms.

### Viewer page

`INDEX_HTML` replaced; no server changes, no new endpoints. The stream fills the
viewport and letterboxes via `object-fit: contain`. The header floats over the
top edge and fades after 3 idle seconds. Frame rate is counted client-side from
the MJPEG element's `load` events — one fires per frame — so it needs no server
support. A static page stops repainting, so the header says "idle" rather than
showing 0 fps as a fault.

### Asset location

`cursor-overlay.js` moved from `tests/fixtures/` to `src/viewer/`. Production
code (`Page::inject_cursor_overlay`) `include_str!`s it, so it was never a
fixture.

## Testing

Unit (run locally):

- `CursorMotion::parse` accepts `smooth`/`instant` and rejects anything else.
- `view` defaults to smooth and `serve` to instant; the flag overrides both.
  Getting these backwards would silently add ~220ms to every CI click.
- The smooth profile's per-step interval stays above the 8ms floor.

Integration (needs a browser, `tests/it_cursor_motion.rs`):

- Instant motion dispatches exactly one `mousemove` before a click.
- Smooth motion dispatches many, and the click still lands.
- Clicking where the cursor already is dispatches none — the double-move
  regression.
- The injected overlay contains the solid fill and dark outline, and no longer
  contains the neon palette or the blur filter.
- `it_viewer.rs` asserts the new index markup and that the page references no
  endpoint the server does not serve.

## Known gap

The browser-backed tests could not be executed in the development container:
Chrome exits with SIGTRAP there regardless of flags, both before and after this
change. `cargo fmt`, `cargo clippy --all-targets --all-features -D warnings`,
and 47 unit tests pass locally. The cursor rendering and the motion event
counts are unverified until CI runs.
