# Architecture

## Layered design

```
AI Coding Agent
      │  CLI  │  JSON-RPC/stdio  │  MCP
      ▼
Agent API Layer  (session: operations, references, validation, trace)
      ▼
Browser Runtime Core
      ├─ Browser Process Manager
      ├─ CDP Transport (WebSocket)
      ├─ Page / Target Manager
      ├─ Input Engine (mouse, keyboard, drag)
      ├─ Observation Engine (DOM extraction, references)
      ├─ Stability Detector (wait)
      ├─ Console/Network Collector
      ├─ Screenshot Engine (capture, annotate, dewiggle)
      ├─ Live Viewer (screencast + MJPEG HTTP + cursor overlay)
      └─ Trace/Replay Engine
      ▼
chrome-headless-shell / Chromium
```

Each layer is independently testable. The `cdp` module is the only place that
knows raw CDP JSON; everything above it works with typed Rust.

## Browser process control

`BrowserProcess` spawns Chrome directly (no driver). It:
- picks a free TCP port for CDP (bound to 127.0.0.1),
- creates a temp user-data-dir removed on exit,
- waits for the `/json/version` HTTP endpoint before declaring ready,
- reaps the child with a bounded blocking thread in `Drop` (no zombies).

[Decision Log]
- 목적과 의도: 완전한 프로세스 생명주기 제어로 좀비/임시파일 잔류 방지.
- 기존 구현 및 제약 조건: 드라이버 기반 런타임은 정리 책임이 불명확.
- 검토한 주요 대안: Playwright/WebDriver 기반 제어.
- 선택한 방식: 직접 `Command::spawn` + CDP WebSocket attach.
- 다른 대안 대신 이 방식을 선택한 이유: 정리 책임 단일화, 의존성 최소화.
- 장점, 단점 및 영향: CDP 프로토콜 처리 부담 증가, 대신 완전한 제어권.

## Stealth mode (`browser/stealth.rs`)

Optional (`--stealth`, `LaunchOptions::stealth`). Off by default, and when off no
stealth code path executes at all. It suppresses the signals that identify
`--headless=new` in three places, because no single place can cover them:

1. **Launch flags** (`StealthProfile::launch_args`) — process-wide, so they also
   apply to frames we never attach to: `--disable-blink-features=Automation
   Controlled` (removes `navigator.webdriver` at the source),
   `--user-agent=…` built from the browser's real version,
   `--enable-unsafe-swiftshader` (keeps WebGL alive on software GL).
   Two normal defaults are dropped here: `--hide-scrollbars` and `--disable-gpu`.
2. **`Emulation.setUserAgentOverride`** on the page session — the only way to fix
   `Sec-CH-UA` client hints and `navigator.userAgentData`, which are generated
   from Chrome's own brand list (`HeadlessChrome`) regardless of `--user-agent`.
   Applied on a page session it covers the whole WebContents, subframes included.
3. **`stealth.js` pre-load script** — patches what is left in JS (WebGL driver
   strings, `navigator.plugins`, `window.chrome`, window/screen geometry,
   notification permission), guarding every patch behind a check of the real
   value, and making every replacement report native source via one wrapped
   `Function.prototype.toString`. It runs *after* `collectors.js` so it can also
   make that file's `fetch`/console wrappers report native source.

Cross-origin iframes are separate CDP targets, so (3) does not reach them —
which is precisely where a challenge widget runs. Stealth therefore turns on
`Target.setAutoAttach` with `waitForDebuggerOnStart` and patches each child
session before resuming it.

Invariant: every auto-attached target must be resumed on every path, including
injection failure. A target left paused hangs the frame that owns it.

## CDP transport

`CdpClient` owns one WebSocket. It assigns monotonic request ids, matches
responses via a pending-request map, and fans out events to a subscriber.
"Flatten" mode attaches a `sessionId` per page so the browser-level connection
serves all pages.

## Input engine

Mouse and keyboard dispatch **real** CDP `Input.*` events. Drag emits
`mouseMoved` along an interpolated path (not teleport), so sliders/canvases/maps
receive intermediate events. Keyboard normalizes key names (`Ctrl`/`Control`,
`Cmd`/`Command`/`Meta`) and uses `Input.insertText` for non-ASCII.

## Observe pipeline

`extract_elements.js` queries the composed tree for interactive elements,
computes accessible name/role/bounds, and returns a compact JSON array. The
Rust side assigns `@eN` references in reading order. References are re-resolved
to **current** coordinates at action time (not cached), so layout shifts within
a generation still resolve.

## Screenshot annotation (`observe/annotate.rs`)

`screenshot --annotate` draws bounding boxes and `@eN` labels straight onto the
captured PNG so a vision-based agent can read exact click coordinates off the
image. PNG container, IHDR and row filters are handled in-crate; only `flate2`
is used, for the zlib stream. CDP screenshots are always 8-bit RGB/RGBA
non-interlaced, so that subset is enough.

## Dewiggle (`observe/dewiggle.rs`)

Reverses per-glyph vertical wobble in animated text CAPTCHAs using **captured
pixels only** — no answer arrays, no framework props, no DOM text. It captures
N frames of a region, computes each column's intensity-weighted vertical ink
centroid per frame, takes the cross-frame average as that column's neutral
baseline, shifts every frame's columns onto their baseline, and averages the
realigned frames. Aligned ink accumulates while noise averages toward gray, so
the glyphs sharpen. Optional `--chars N` segments the result into equal-width
glyph bands and saves per-glyph crops. The page is never mutated (no clock
freezing, no script injection), which is why realignment is done offline on
pixels rather than by controlling the animation.

## Live viewer (`viewer/`)

On headless Linux there is no visible OS cursor for `Input.dispatchMouseEvent`,
so an agent's clicks and drags are invisible. `headless-use view` fixes both
halves of that:

- `viewer/screencast.rs` subscribes to CDP `Page.startScreencast` /
  `Page.screencastFrame`, decodes each base64 JPEG, acks it (Chrome stops
  sending after ~2 unacked frames), and keeps **only the newest** frame in a
  watch channel — backpressure drops stale frames rather than queuing them.
  `Page.frameNavigated` restarts the screencast, and an idle re-arm timer
  re-issues `startScreencast` when no frame has arrived for a while, because
  Chrome only emits frames on repaint (static pages and iframe-heavy sites
  otherwise freeze the stream).
- `viewer/http.rs` is a hand-written HTTP/1.0 server on a tokio `TcpListener`
  (no axum/hyper dependency for one endpoint): `/` serves a small HTML index,
  `/stream` serves `multipart/x-mixed-replace` MJPEG pulled from the shared
  `Screencast`.
- `viewer/cursor-overlay.js` is injected into the page (via
  `Page::inject_cursor_overlay`) so the synthetic cursor position is visible in
  the stream. It ships inside the binary through `include_str!`.

The viewer binds to `127.0.0.1:7780` by default. `--viewer-host` can widen that
for remote viewing. Access is gated by a token carried as `?token=…`, generated
at startup unless pinned with `--viewer-token`: **required** on a non-loopback
bind, optional on loopback. The stream carries whatever the page shows,
including logged-in content, and is plain HTTP — see
[security.md](security.md#access-token) for what the token does and does not
protect against. The CDP endpoint is unaffected and stays loopback-only.

## Session lifecycle

`Session` owns a `Browser` + active `Page`. `serve` keeps it alive and dispatches
JSON-RPC; `run` creates one, performs one action, and tears down. Tracing wraps
every action into `actions.jsonl` + `report.html`.
