# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- Host allow/deny policy for navigation.
