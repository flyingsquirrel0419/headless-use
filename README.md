# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)


> Computer use for web development agents, built for headless Linux and CI.

`headless-use` is a lightweight browser runtime that lets AI coding agents **see,
use, and debug** the web apps they build. It drives Chrome over the Chrome DevTools
Protocol (CDP) with **real input events** — not JavaScript `element.click()` — and
gives agents both screenshot-based "computer use" and token-light semantic
references (`@e1`, `@e2`, …) to page elements.

It is a single Rust binary with no Node.js runtime, designed to run on GUI-less
servers, Docker, and CI without Xvfb.

---

## Why

Browser automation tools are built for **test scripts**. Agents need **real-time
interaction**: they observe a page, decide the next action, and verify the result.
Standard tools are selector-centric, DOM-only, and leave the agent to re-screenshot
on every step. `headless-use` is agent-centric:

| General browser automation            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| Writing test code                     | Real-time agent operation                 |
| CSS selectors                         | Coordinates **and** semantic references  |
| DOM only                              | Screenshot + DOM + Console + Network       |
| Node.js dependency common             | Single Rust binary                        |
| Local desktop focused                 | Headless Linux, Docker, CI first          |
| Result only                           | Session trace, diagnostic report |

## 30-second Quick Start

```bash
# Build (single binary, no runtime deps)
cargo build --release

# Diagnose your environment
./target/release/headless-use doctor

# Start a long-lived session (JSON-RPC over stdio)
./target/release/headless-use serve --no-sandbox
```

In another terminal, send JSON-RPC requests to the running session:

```bash
# Open a page, observe interactive elements, click by reference, type.
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe` returns a compact list of interactive elements with stable references:

```
[@e1] textbox "이메일"
[@e2] textbox "비밀번호"
[@e3] button "로그인"
[@e4] link "회원가입"
[@e5] checkbox "" [unchecked]
```

## One-shot mode

```bash
# Open a URL and save a screenshot, then exit.
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## Install

### From source

```bash
cargo install --path .
# Requires a Chrome/Chromium on PATH, or set HEADLESS_USE_BROWSER_PATH.
```

### Browser discovery

`headless-use` looks for a browser in this order:

1. `HEADLESS_USE_BROWSER_PATH` env var
2. `chrome-headless-shell`, `chromium`, `google-chrome`, `google-chrome-stable` on `PATH`

Override explicitly:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -t headless-use .
# The image bundles Chromium. Run one-shot:
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **Sandbox note:** Chromium refuses to run as root without `--no-sandbox`. In
> Docker CI this is acceptable for isolated builds; for trusted production use
> prefer running as a non-root user (the shipped Dockerfile does) and keep the
> sandbox enabled. See [docs/security.md](docs/security.md).

## Supported features

- **Real input**: `Input.dispatchMouseEvent`, `dispatchKeyEvent`, `insertText`
- **Mouse**: move, click (left/right/middle/back/forward), double/triple, down/up, hold, hover, wheel scroll, drag (interpolated), drag-path
- **Keyboard**: down/up/press, chords (`Control+Shift+P`), type, insert-text (CJK/emoji safe), hold, repeat
- **Observe**: DOM-based interactive-element extraction, semantic `@g<gen>:eN` references (generation-bound for stale detection), bounding boxes, stale-reference detection on any navigation
- **Diagnostics**: console + uncaught errors, network (CDP `Network.*` events — not JS monkey-patching) with secret masking, wait-until-stable (activity-timestamp based, catches sub-poll requests)
- **Screenshots**: viewport, full-page (element-region capture is on the roadmap)
- **Sessions**: long-lived `serve` (JSON-RPC stdio), one-shot `run`, trace + report
- **Trace**: `actions.jsonl`, `report.html` (self-contained), forced secret redaction at the writer boundary
- **MCP server**: spec-compliant `initialize`/`tools/list`/`tools/call` over stdio

## MCP server

`headless-use mcp` runs a spec-compliant MCP server (protocolVersion `2024-11-05`)
over stdio. AI agents connect directly without wrapping JSON-RPC:

```bash
headless-use mcp --no-sandbox
```

The server advertises 18 `browser_*` tools with typed `inputSchema`. Screenshot
results return as MCP image blocks; everything else returns compact JSON text
blocks. Errors return `isError: true` with a recovery hint.

### Claude Desktop / Cursor config

Add to your MCP client config:

```json
{
  "mcpServers": {
    "headless-use": {
      "command": "/usr/local/bin/headless-use",
      "args": ["mcp", "--no-sandbox"]
    }
  }
}
```

### Protocol flow

```
client → initialize {protocolVersion, capabilities}
server ← {protocolVersion, capabilities, serverInfo}
client → notifications/initialized
client → tools/list
server ← {tools: [...18 browser_* tools...]}
client → tools/call {name: "browser_observe", arguments: {}}
server ← {content: [{type:"text", text:"{...elements...}"}], isError: false}
```

## CLI reference

```
headless-use
├── launch      Launch a browser and keep it running
├── serve        Start a long-lived JSON-RPC session over stdio
├── run          Run a one-shot action and exit
├── doctor       Diagnose the environment
├── install-browser   Print browser install guidance
└── mcp          Start the MCP server over stdio
```

`serve` accepts JSON-RPC methods including: `browser.open`, `observe`, `screenshot`,
`click`, `hover`, `mouse.move`, `mouse.down`, `mouse.up`, `scroll`, `mouse.drag`,
`mouse.drag_path`, `type`, `insert-text`, `key.press`, `key.down`, `key.up`, `wait`,
`console`, `network`, `browser.close`. Add `--json` to launch/run for machine output.

## Error model

Errors are structured so an agent can decide recovery. Example:

```json
{
  "id": 14,
  "error": {
    "code": "STALE_REFERENCE",
    "message": "stale reference @e3",
    "recovery": "Reference @e3 is stale. Run `headless-use observe` again and use the new reference."
  }
}
```

Error codes: `BROWSER_NOT_FOUND`, `LAUNCH_FAILED`, `CONNECTION_FAILED`,
`PROTOCOL_ERROR`, `TIMEOUT`, `TARGET_CLOSED`, `ELEMENT_NOT_FOUND`,
`ELEMENT_NOT_INTERACTABLE`, `STALE_REFERENCE`, `INVALID_INPUT`.

## Docker example

```bash
docker build -t headless-use .
docker run --rm --network host --shm-size=1g headless-use \
  serve --no-sandbox
```

## Limitations

- Cross-origin iframe interaction is limited; documented errors are returned.
- OS-level IME composition is not fully emulated; CJK/emoji use `Input.insertText`.
- Firefox/WebKit are out of initial scope (Chromium only).
- Automatic browser download (`install-browser`) prints guidance rather than fetching.
- Touch input (`touch tap`, `touch swipe`) is structurally supported but not in the MVP CLI.

## How it differs from Playwright/Puppeteer

`headless-use` does **not** wrap Playwright. It speaks CDP directly over WebSocket,
owns process lifecycle (temp-profile cleanup, zombie prevention, signal handling),
and exposes an agent-first API (references + observe + structured errors). It can
be used alongside Playwright for compatibility checks, but is not a Playwright
wrapper.

## Roadmap

- `report.html` interactive timeline
- `replay` CLI with deterministic action re-execution
- HTML5 file drop with MIME handling
- `install-browser` with checksum verification

## Security

See [docs/security.md](docs/security.md). Key points: CDP binds to `127.0.0.1`
only, secrets are masked in traces (including auto-detection of password fields),
and file-path traversal is rejected. A host allow/deny policy scaffold exists in
`src/security/` but is **not yet wired to navigation** (on the roadmap).

## Community

- [Contributing](CONTRIBUTING.md) — development setup, code standards, PR process
- [Code of Conduct](CODE_OF_CONDUCT.md) — community standards
- [Security Policy](SECURITY.md) — vulnerability reporting
- [Changelog](CHANGELOG.md) — release history
- [Discussions](https://github.com/headless-use/headless-use/discussions) — questions & ideas

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
