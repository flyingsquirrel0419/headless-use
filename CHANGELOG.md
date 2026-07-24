# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
