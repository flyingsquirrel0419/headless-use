# Live viewer: streaming quality tuning and page redesign

Date: 2026-07-26
Status: approved

## Problem

Three gaps in the `view` command's live stream.

1. **Latency.** The MJPEG socket is written with Nagle's algorithm enabled, so
   small frame parts can sit in the kernel waiting to coalesce. For a
   latency-sensitive live view this is pure loss.
2. **`--fps` is dead.** `ViewArgs::fps` parses and defaults to 30 but is never
   read. Its own doc comment claims it "scales max dims", which it does not.
   Users who set it get no change and no error.
3. **The viewer page is minimal.** The floating bar shows only fps and stream
   dimensions. There is no way to see which URL or page the agent is on
   without reading the terminal. Dark-only styling; no light mode.

## Decisions

### Server: latency

`set_nodelay(true)` on each accepted connection in `http.rs`. One line;
removes Nagle-induced buffering on frame writes.

### Server: frame-rate throttle (`--fps` wired for real)

`ViewerOptions` gains `min_frame_interval: Duration`. `write_stream` skips a
frame when it arrives sooner than the interval since the last write on that
connection. Per-connection, not in `Screencast`: the watch channel already
keeps only the latest frame, so skipping a send loses nothing — the next send
picks up the newest frame (natural backpressure). `--fps N` maps to an
interval of `1/N` seconds; default stays 30. The flag's doc comment is
rewritten to say what it actually does.

Rejected: throttling in `Screencast` via `everyNthFrame` — that skips frames
globally for all consumers and counts repaints, not wall time, so "15 fps"
would not mean 15 frames per second.

### Server: `/status` endpoint

New endpoint returning JSON:

```json
{ "url": "...", "title": "...", "viewport": { "width": 1280, "height": 720 }, "quality": 80 }
```

`commands.rs` spawns a task that polls the page's URL and title every 2s and
writes them into an `Arc<RwLock<StatusInfo>>` passed to `serve()`. Read-only
CDP calls — the no-page-DOM-mutation rule holds. On a poll error the last
known value stays; the panel never shows an error state for a transient poll
miss. The handler serves the current snapshot; `Content-Type:
application/json`, `Cache-Control: no-store`.

### Viewer page: status panel

Top bar rebuilt. Left: connection dot, page title, URL in an ellipsized pill.
Right: stream resolution, fps, JPEG quality. The page polls `/status` every
2s and fills the title/URL; fps and resolution stay client-computed from the
MJPEG element's load events as today. Idle fade behavior unchanged.

### Viewer page: theme

CSS custom properties for all colors; `prefers-color-scheme` switches the
palette. Dark keeps the current tones. Light uses a bright neutral background
with softened shadows. A subtle radial gradient behind the stage in both —
decoration stays behind the stream, never over it.

## Error handling

- `/status` poll failure: keep last value, no stale badge (noise).
- Stream end: unchanged — "stream ended" text and amber dot.

## Testing

Unit:

- Throttle interval math: `--fps 15` → ≥66ms between writes; default 30.
- `ViewArgs` fps default and override parse.
- Routing: `/status` reaches the status handler; unknown paths still 404.

Integration (`it_viewer.rs`):

- New index markup markers present (status pill, CSS variables).
- `/status` returns well-formed JSON with the four fields.
- Every endpoint the page references is served (no dead fetch).
