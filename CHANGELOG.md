# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — stealth mode (`--stealth`)

- **`--stealth` keeps `--headless=new` but stops it announcing itself**, so sites
  behind a bot check (Cloudflare Turnstile and friends) stop serving a challenge
  that only headless browsers get. Available on `launch`, `serve`, `run`, `view`
  and `mcp`, and as `LaunchOptions::stealth`. Off by default; when off, no
  stealth code runs.
- Three layers, because no single one covers everything:
  - **Launch flags** (process-wide, so they reach frames we never attach to):
    `--disable-blink-features=AutomationControlled` removes
    `navigator.webdriver` at the source rather than patching it from JS, and
    `--user-agent` — built from the browser's own reported version — drops
    `HeadlessChrome/…` from every request.
  - **`Emulation.setUserAgentOverride`** per page: the only way to fix
    `Sec-CH-UA` client hints and `navigator.userAgentData`, which Chrome
    generates from its own brand list (`HeadlessChrome`) no matter what
    `--user-agent` says. Applied on a page session it covers the whole
    WebContents, subframes included.
  - **A pre-load script** for the JS-only surface: SwiftShader WebGL driver
    strings, an empty `navigator.plugins`, a missing `window.chrome`,
    `outerHeight == innerHeight`, a screen the size of the window, and
    `notifications: denied`. Every patch is guarded by a check of the real value,
    and every replacement function reports native source through a single wrapped
    `Function.prototype.toString` — an unnecessary override is one more accessor
    a detector can catch lying.
- **Cross-origin iframes are covered.** They are separate CDP targets, so a page's
  pre-load script never reaches them — and that is exactly where a challenge
  widget runs its fingerprinting. Stealth enables `Target.setAutoAttach` with
  `waitForDebuggerOnStart` and patches each child session before resuming it.
- **This tool's own instrumentation is no longer a signal.** `collectors.js` wraps
  `fetch`, XHR and the console methods and parks buffers on `window`; a wrapped
  `fetch.toString()` is not native source. The stealth script (registered after
  the collector, so it runs after it) restores native source for those wrappers
  and makes the `__hu_*` buffers non-enumerable.
- **Browser choice matters, so stealth changes it**: `chrome-headless-shell` is
  tried last, since it ships without `window.chrome`, PDF plugin entries or
  proprietary codecs — properties JS cannot fake convincingly. A shell is still
  used if it is all that is installed, with a warning.
- **Two launch defaults change under `--stealth`**: scrollbars are no longer
  hidden (a zero-width scrollbar shows up as `innerWidth == clientWidth`, a
  documented check), and `--disable-gpu` is dropped with
  `--enable-unsafe-swiftshader` added so WebGL still exists — a page with no
  WebGL context is a louder signal than a software renderer.
- Tests: `tests/js/stealth_dom.mjs` runs the real pre-load script against a
  worst-case fake headless DOM under `node` (skipped when `node` is absent) and
  asserts both the patched values and that the patches report native source;
  browser-level tests assert what a page actually observes, including that an
  auto-attached cross-origin frame is resumed rather than left paused.

### Changed — live viewer

- **New cursor design.** The overlay is now a solid white pointer with a dark
  outline and a soft drop shadow, at roughly OS cursor size (28px, down from
  48px). The previous cyan arrow with a blurred glow halo covered a meaningful
  part of whatever control was being clicked and read as decoration. Click
  feedback is a single expanding ripple instead of a cyan ring held for the
  whole press.
- **The cursor travels to its target.** `Session` takes a `CursorMotion`
  setting; `headless-use view` defaults to `smooth`, which eases the cursor
  along the path and dispatches real intermediate `mouseMoved` events — visible
  in the stream, and enough to open hover menus that require movement.
  `serve`/`run`/`mcp` keep the previous instant jump, since travel time is dead
  weight for headless automation. `--cursor-motion smooth|instant` overrides
  either default.
- `Mouse::click` no longer re-issues a move to a target the cursor already
  occupies, which would otherwise add a phantom `mouseMoved` to the event
  stream and to traces.
- **Viewer page rebuilt.** The stream now fills the window and letterboxes via
  `object-fit: contain` instead of overflowing the fold at 1280x720. The header
  floats over the top edge, shows a live frame rate derived from the MJPEG load
  events, distinguishes "idle" (a static page stops repainting) from
  "stream ended", and fades out while you watch. No new server endpoints.
- `cursor-overlay.js` moved from `tests/fixtures/` to `src/viewer/`. It ships
  inside the binary via `include_str!`, so production code was reaching into
  the test tree for an asset.

### Fixed — correctness

- **Replay silently skipped every click.** `Session::record_action` writes the
  action type `mouse.click`, but the replay dispatch table matched `"click"`.
  Recorded clicks fell through to the catch-all arm, were never executed, and
  were still counted as successes — a replay of a click-driven trace reported
  `all_succeeded: true` without clicking anything. Replay now accepts the
  recorded name, and an action type it does not know how to dispatch is
  reported as **skipped** rather than as a silent success.
- **Network requests never recorded completion.** `Network.responseReceived`
  moved the request straight into the history, so `pending()` dropped to zero
  the moment response headers arrived (while the body was still streaming) and
  the later `loadingFinished` found nothing to update. `finished` stayed
  `false` and `duration_ms` stayed `None` for every successful request.
  Requests now stay in flight until `loadingFinished`/`loadingFailed`, and
  `duration_ms` is measured locally instead of from CDP's monotonic
  `timestamp` (which was being reported as a duration).
- **Element screenshots captured the wrong region on a scrolled page.**
  `getBoundingClientRect()` is viewport-relative but `Page.captureScreenshot`'s
  `clip` is document-relative; the scroll offset is now added. For a `@eN`
  target the referenced element's own box is used — resolving via
  `elementFromPoint` returned the topmost descendant, clipping to an inner
  element instead of the referenced one.
- **`reload` was a re-navigation**, not a reload: it read the current URL and
  called `Page.navigate`, dropping the history entry's POST body. It now issues
  `Page.reload` and waits for the document to actually swap.

### Fixed — security

- **Agent-supplied file paths are now validated.** `util::validate_path_within`
  existed but was never called, while the README and `docs/security.md` claimed
  traversal was rejected. `trace.start` (`base`) and `replay` (`runDir`) take
  paths straight from the agent; both are now confined to the working
  directory. Operator-supplied CLI paths are deliberately unaffected.
- **Host policy no longer ignores hostless URLs.** `file:`, `data:`,
  `javascript:` and `chrome:` URLs have no host, so every allow/deny pattern
  missed them — `--deny-host` alone did not stop `file:///etc/passwd`. Once any
  host policy is configured, navigation is restricted to `http`/`https` (plus
  `about:blank`). With no policy configured the default stays permissive.
- **Site isolation is no longer disabled.** `IsolateOrigins` and
  `site-per-process` were in `--disable-features`; only `Translate` is now.
- **Viewer exposure is documented and warned about.** `--viewer-host` (added
  in an earlier change) can bind the unauthenticated MJPEG stream to all
  interfaces while the module doc still claimed loopback-only. The contradiction
  is resolved, `README.md` and `docs/security.md` describe the exposure, and a
  warning is printed whenever the bind address is not loopback.
- Report HTML now escapes quotes and the action-type field.

### Fixed — robustness

- Browser cleanup took the child-process lock with `try_lock` and silently
  skipped the kill on contention, leaking the process and its temp profile.
- `CdpClient::subscribe_events` and `Session::with_policy` used
  `Mutex::blocking_lock`, which panics inside a tokio runtime. Both are removed
  in favor of the existing `_async` variants.
- Xvfb picked a display number at random without checking whether one was in
  use, and cleaned up with `pkill -f "Xvfb :N"`, which could kill an unrelated
  X server. It now picks a free display and kills by pid.
- An invalid `--viewer-host` panicked instead of returning an error, and the
  viewer replied `HTTP/1.0 404 OK`.
- The viewer read the HTTP request with a single `read`, so a request split
  across TCP segments was routed to 404.
- `Screencast` now stops on drop instead of leaving Chrome encoding frames.

### Changed

- `wait` polls with one `Runtime.evaluate` per iteration instead of three.
- `CdpClient::call` and `call_session` share one implementation.
- Secret-key detection matches word segments instead of raw substrings, so
  `author`, `monkey` and `keyboard` are no longer redacted.
- Network history uses a `VecDeque` (O(1) trim instead of O(n)).
- Removed `Session::ensure_network_tracker`, a no-op that always returned `Ok`.
- Test fixture server answers 404 for missing fixtures instead of 200.

### Added — replay engine
- **Replay engine** (`src/trace/replay.rs`): reads a recorded `actions.jsonl`
  and re-executes each action (`open`, `click`, `type`, `insert-text`,
  `key.press`, `scroll`, `mouse.drag`, `hover`, `screenshot`, `wait`, `reload`)
  against a fresh session. Re-observes before ref-resolving actions and after
  navigation so generation-bound references re-resolve. Sensitive (redacted)
  values are skipped gracefully. Returns a detailed `ReplayResult` with
  per-step success/failure and the failure stop point.
- **`replay` CLI command**: `headless-use replay <run-dir>` re-runs a trace.
  `--json` for machine-readable output.
- **`trace.start`/`trace.stop` JSON-RPC + MCP**: start/stop tracing at runtime
  without rebuilding the session. The trace field is now behind a lock so it
  can be swapped at runtime.
- **MCP tools**: `trace_start`, `trace_stop`, `replay` added to the MCP schema.
- **4 replay integration tests** verify record→replay fidelity, redacted-value
  skipping, and runtime trace start/stop.

### Added — P1 features (document-feature consistency)
- **Host allow/deny policy enforced on navigation**: `open`/`goto`/`reload` now
  consult the `Policy` and return `NAVIGATION_BLOCKED` before any request reaches
  the network for disallowed hosts. Configurable via `--allow-host`/`--deny-host`
  on `serve`/`mcp`/`run`, or `Session::with_policy`. Deny list takes precedence.
- **Element-region screenshots**: `screenshot` accepts an `element` reference
  (`@g<gen>:e<num>`) to capture only that element's bounding box via
  `elementFromPoint` + clip. Available in CLI (`--element`), JSON-RPC, and MCP
  (`browser_screenshot`). One-shot `run` auto-observes to resolve the reference.
- **Trace screenshot saving + report embedding**: screenshots are saved into the
  run's `screenshots/` directory and embedded inline as base64 data URIs in
  `report.html`, so the report is fully self-contained with no external images.

### Fixed — P0 release blockers (round 5)
- **Network wait no longer misses fast requests**: `wait` now uses a
  `last_network_activity` timestamp updated on every CDP `Network.*` event
  (started/response/finished/failed), not a 50ms poll of the in-flight count.
  A request that starts and finishes between polls still resets the idle clock.
- **`browser_network` now uses CDP events**: replaced the JS `fetch`/`XHR`
  monkey-patch (which double-recorded failed XHRs and missed resource loads)
  with a single CDP-derived request history shared by `wait` and
  `browser_network`.
- **Canonical generation-bound references**: observe output now includes a
  `ref` field per element of the form `@g<gen>:e<num>`, and compact output uses
  the real generation (was hardcoded to `1`). Agents no longer need to combine a
  separate `generation` field with `ref_id`.
- **References invalidated on any navigation**: `nav_generation` is now
  incremented by a `Page.frameNavigated` listener, so button-click navigation,
  form submit, redirect, `location.href`, reload, and history back/forward all
  invalidate stale references — not just explicit `open()`.
- **Transactional mouse button state**: `down`/`up`/`click` now commit the
  shared button mask only after a successful CDP dispatch, so a failed dispatch
  cannot leave local state inconsistent with the browser.
- **Drag carries cumulative button state**: `drag`/`drag_path` intermediate
  moves report the cumulative held-buttons mask (e.g. `buttons=3` when right is
  held during a left drag), while keeping `button` set to the dragged button so
  Chrome pointer-capture stays active for native controls (sliders).
- **Unknown mouse button is an error**: RPC `parse_button` now returns
  `INVALID_INPUT` for unknown button strings (e.g. `"rgiht"`) instead of
  silently falling back to a left click.
- **Forced secret redaction at the trace boundary**: `Trace::record` applies
  `mask_json` to every action's params as a final defense, so secrets are
  redacted even if a caller forgets `sensitive=true`.
- **`type` auto-detects password fields**: `type_text` now checks whether the
  focused element is a password field and redacts the trace automatically,
  matching the existing `insert_text` behavior.
- **Console messages masked**: console text and source URLs are now run through
  secret masking before being returned.
- **CLI auto-enables `--no-sandbox` for root**: `serve`/`mcp`/`launch`/`run`
  now call `with_no_sandbox_for_root()` so the CLI works in Docker/CI without a
  manual flag (matching `doctor`).

### Changed
- CDP event subscription is now a broadcast bus (multiple subscribers) instead
  of a single replaceable subscriber, so the Network tracker and the
  `Page.frameNavigated` listener coexist.
- Documentation corrected to remove overstated claims (replay, element
  screenshot, trace screenshots, AX tree, host-policy enforcement).

## [0.1.0] - 2026-07-24

### Added
- Initial release: lightweight headless Computer Use browser runtime in Rust.
- Chrome launch + CDP transport (WebSocket, request/response matching, events).
- Real input engine: mouse (move/click/down/up/scroll/drag/drag-path) and
  keyboard (down/up/press/type/insert-text/hold/repeat) via `Input.*` CDP events.
- Observation engine with semantic `@eN` references and stale detection.
- Console + network collection with secret masking; wait-until-stable.
- CLI (`launch`, `serve`, `run`, `doctor`, `install-browser`, `mcp`).
- JSON-RPC over stdio protocol with structured error codes + recovery hints.
- MCP server (protocolVersion `2024-11-05`): `initialize`/`tools/list`/`tools/call`
  with 18 typed `browser_*` tools and image content blocks for screenshots.
- Trace recording (`actions.jsonl`, `report.html`) with redaction.
- 16 local fixture sites + 53 integration/E2E tests.
- Dockerfile (non-root, Chromium bundled) and GitHub Actions CI/release.
- Multilingual README: English, 한국어, 日本語, 中文.
- Community health files: CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md,
  GitHub issue forms, PR template.
- Docs: architecture, input-model, protocol, security, troubleshooting, examples.

### Security
- CDP endpoint bound to `127.0.0.1` only.
- Secret masking in traces and network URLs (query-string redaction).
- Path traversal validation for screenshot output paths.
- Host allow/deny policy scaffold (not yet wired to navigation).
