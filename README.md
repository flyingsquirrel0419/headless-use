# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)


> Computer use for web development agents, built for headless Linux and CI.

`headless-use` is a lightweight browser runtime that lets AI coding agents **see,
use, and debug** the web apps they build. It drives Chrome over the Chrome DevTools
Protocol (CDP) with **real input events** — not JavaScript `element.click()` — and
gives agents both screenshot-based "computer use" and token-light semantic
references (`@g1:e1`, `@g1:e2`, …; `@eN` accepted as shorthand) to page elements.

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
```

`serve` starts a long-lived session speaking newline-delimited JSON-RPC on
stdin/stdout (stdout carries **only** protocol responses; logs and banners go
to stderr). Pipe requests into one process — each `serve` owns its own browser:

```bash
# Open a page, observe interactive elements, click by reference, type.
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@g1:e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe` returns page metadata plus the interactive elements, each with a
generation-bound reference (`ref`) that `click`, `hover`, and `screenshot`
accept as a target:

```json
{
  "id": 2,
  "result": {
    "schemaVersion": 1,
    "page": { "url": "http://localhost:3000/", "title": "로그인", "viewport": { "width": 1280, "height": 720 } },
    "elements": [
      { "ref": "@g1:e1", "ref_id": 1, "role": "textbox", "name": "이메일" },
      { "ref": "@g1:e2", "ref_id": 2, "role": "textbox", "name": "비밀번호" },
      { "ref": "@g1:e3", "ref_id": 3, "role": "button",  "name": "로그인" }
    ],
    "generation": 1
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

(Elements carry more fields — bounding box, `visible`, `enabled`, `checked`,
`value`, `selectorHint` — trimmed here for brevity. The bare `@eN` form is
accepted as shorthand, but only the full `@g<gen>:eN` form detects stale
references after navigation.)

Two costs are opt-in to keep the default path light:

- `observe` with `"listeners": true` additionally detects
  programmatically-attached click listeners via CDP (up to two extra CDP
  round-trips per candidate element).
- `click` with `"effects": true` samples post-click effects (`dom_mutations`,
  `network_requests`, `navigated`, `focus_changed`) for 300 ms; without it,
  `effects` is `null` and only the cheap pre-click hit test runs.

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
2. `chrome-headless-shell`, `chromium-headless-shell`, `chromium`,
   `chromium-browser`, `google-chrome`, `google-chrome-stable` on `PATH`

With `--stealth` the order is inverted: the headless shells are tried last,
because they are missing browser APIs that bot checks read (see
[Stealth mode](#stealth-mode)).

Override explicitly:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -f docker/Dockerfile -t headless-use .
# The image bundles Chromium and runs as a non-root user with WORKDIR
# /home/hu. Mount a writable directory for outputs (mkdir -p output first):
docker run --rm --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  -v "$PWD/output:/home/hu/output" \
  headless-use \
  run --url http://127.0.0.1:3000 --screenshot output/page.png --no-sandbox
```

> **Sandbox note:** Chromium refuses to run as root without `--no-sandbox`;
> when the process runs as root (common in CI containers), `headless-use`
> auto-applies it. The shipped Dockerfile runs as a non-root user, where the
> default Docker seccomp profile can still interfere with Chromium's sandbox —
> hence `--security-opt seccomp=unconfined` plus `--no-sandbox` above, matching
> the shipped `docker/docker-compose.yml`. For trusted production use prefer
> keeping the sandbox on and adjusting seccomp instead. See
> [docs/security.md](docs/security.md).

## Stability: what v1 guarantees

The **core browser-control API is stable** as of v1.0.0 and follows semver:

- CLI subcommands `serve`, `run`, `mcp`, `launch`, `doctor`, `install-browser`
- JSON-RPC methods `browser.open`/`page.goto`, `observe`, `click`, `hover`,
  `mouse.*`, `scroll`, `type`, `insert-text`, `key.*`, `wait`, `screenshot`,
  `console`, `network`, `evaluate`, `url`, `title`, `browser.close`
- The corresponding MCP `browser_*` tools
- The error model (codes are stable strings) and the response envelope
  (`jsonrpc`, `schemaVersion` — result shapes only gain fields)

**Experimental** — usable today, but shape and defaults may change in minor
releases, and they are not covered by the v1 stability guarantee:

- **Live viewer** (`view`, `--viewer-*` flags, the MJPEG stream)
- **Trace + Replay** (`trace.start`/`trace.stop`/`replay`, `actions.jsonl`,
  `report.html`)
- **Stealth mode** (`--stealth`) — fingerprint suppression is an arms race by
  nature
- **Dewiggle** (`dewiggle`, the `dewiggle` method/tool)

## Supported features

- **Real input**: `Input.dispatchMouseEvent`, `dispatchKeyEvent`, `insertText`
- **Mouse**: move, click (left/right/middle/back/forward), double/triple, down/up, hold, hover, wheel scroll, drag (interpolated), drag-path
- **Keyboard**: down/up/press, chords (`Control+Shift+P`), type, insert-text (CJK/emoji safe), hold, repeat
- **Observe**: DOM-based interactive-element extraction, semantic `@g<gen>:eN` references (generation-bound for stale detection), bounding boxes, stale-reference detection on any navigation; opt-in listener scan (`"listeners": true`) promotes elements with programmatically-attached click handlers and flags `opaqueInteractive` surfaces
- **Click reports**: every click returns a pre-click hit test; opt-in (`"effects": true`) post-click effect sampling detects dead clicks
- **Diagnostics**: console + uncaught errors, network (CDP `Network.*` events — not JS monkey-patching) with secret masking, wait-until-stable (activity-timestamp based, catches sub-poll requests)
- **Screenshots**: viewport, full-page, element-region (`--element @g1:e3`)
- **Dewiggle** *(experimental)*: reverse per-glyph vertical wobble in animated text CAPTCHAs using **pixels only** — no answer arrays, no DOM text/props. Captures N frames, realigns each column to its neutral baseline, and averages them into a sharpened image plus optional per-glyph crops. `headless-use dewiggle --url ... --out out.png --chars 6`
- **Stealth** *(experimental)*: `--stealth` keeps `--headless=new` but stops it announcing itself — see [Stealth mode](#stealth-mode)
- **Sessions**: long-lived `serve` (JSON-RPC stdio), one-shot `run`
- **Trace + Replay** *(experimental)*: `actions.jsonl`, `report.html` (self-contained, screenshots embedded), forced secret redaction at the writer boundary, and `replay` to re-execute a recorded trace
- **MCP server**: spec-compliant `initialize`/`tools/list`/`tools/call` over stdio

## MCP server

`headless-use mcp` runs a spec-compliant MCP server (protocolVersion `2024-11-05`)
over stdio. AI agents connect directly without wrapping JSON-RPC:

```bash
headless-use mcp --no-sandbox
```

The server advertises 19 `browser_*` tools with typed `inputSchema`. Screenshot
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
server ← {tools: [...19 browser_* tools...]}
client → tools/call {name: "browser_observe", arguments: {}}
server ← {content: [{type:"text", text:"{...elements...}"}], isError: false}
```

## CLI reference

```
headless-use
├── launch       Launch a browser and keep it running
├── serve        Start a long-lived JSON-RPC session over stdio
├── run          Run a one-shot action and exit
├── dewiggle     (experimental) Capture an animated text region and reverse per-glyph wobble (pixels only)
├── view         (experimental) Serve a live viewer + JSON-RPC session (see below)
├── replay       (experimental) Re-execute a recorded trace from a run directory
├── doctor       Diagnose the environment
├── install-browser   Print browser install guidance
└── mcp          Start the MCP server over stdio
```

```bash
# Reverse the wobble in an animated text CAPTCHA, saving 6 per-glyph crops.
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### Stealth mode (experimental)

Sites behind a bot check (Cloudflare Turnstile and friends) hand headless Chrome
a challenge that headful Chrome walks straight past. `--stealth` closes the gap
without paying for a real display — it works on `launch`, `serve`, `run`, `view`
and `mcp`:

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

What it changes, in the order that matters:

| Layer | Signal removed |
| --- | --- |
| Launch flags | `navigator.webdriver` (via `--disable-blink-features=AutomationControlled`), `HeadlessChrome/…` in the UA string |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"` client-hint headers and `navigator.userAgentData` — the UA flag alone does **not** fix these |
| Pre-load script | SwiftShader WebGL driver strings, empty `navigator.plugins`, missing `window.chrome`, `outerHeight == innerHeight`, a screen the size of the window, `notifications: denied` |
| Auto-attach | The same treatment inside cross-origin iframes, which is where a challenge widget runs |

The user agent is derived from the browser's own version, so the UA, the
client-hint brand list, and the engine all agree. Every replacement function
reports native source (`function … () { [native code] }`), including this tool's
own console/network collectors — a wrapped `fetch` is itself a signal.

Notes:

- Stealth prefers a **full** Chrome/Chromium over `chrome-headless-shell`. The
  shell has no `window.chrome`, no PDF plugin entries, and no proprietary codecs;
  those cannot be faked convincingly. If only a shell is present it is used, with
  a warning.
- Two defaults change under `--stealth`: scrollbars are no longer hidden (a
  zero-width scrollbar is a documented check) and the GPU process stays up so
  WebGL exists (no WebGL at all is louder than a software renderer).
- Fingerprint suppression is an arms race. This removes the signals that make a
  headless browser trivially identifiable; it is not a guarantee against every
  detector. If a site still challenges you, `--compat xvfb` runs a real headful
  browser under Xvfb at roughly double the memory.

### Live viewer (experimental)

```bash
headless-use view --no-sandbox          # prints http://127.0.0.1:7780/?token=…
```

`view` behaves exactly like `serve` (JSON-RPC on stdio) and additionally serves
an MJPEG stream of the page with the agent cursor overlay.

**Access token.** Every run gets a token, printed as part of the URL on stderr
(stdout is the JSON-RPC channel) — open that URL as printed. Pin it with
`--viewer-token <TOKEN>` when the URL has to stay stable. Whether it is
*enforced* depends on the bind address:

| `--viewer-host` | Request without a valid `?token=` |
| --- | --- |
| loopback (`127.0.0.1`, the default) | served — the token is accepted but optional |
| anything else (`0.0.0.0`, a LAN address) | `401 Unauthorized` |

```bash
headless-use view --viewer-host 0.0.0.0 --viewer-token "$(openssl rand -hex 16)"
```

**Cursor motion.** `view` defaults to `--cursor-motion smooth`: the cursor walks
to a click/hover target, emitting real intermediate `mouseMoved` events. That is
what makes the stream readable, and it also drives hover menus that need actual
movement. It costs the travel time (~220ms) per click, so `serve`/`run`/`mcp`
default to `instant`. Override either way:

```bash
headless-use view  --cursor-motion instant   # fastest, cursor teleports
headless-use serve --cursor-motion smooth    # slower, hover-menu friendly
```

> **Exposure note:** the viewer binds to `127.0.0.1` by default.
> `--viewer-host 0.0.0.0` opens it to the network, where the token is required.
> The token is a bearer credential in a URL, so it lands in shell and browser
> history and in `Referer` headers, and the stream itself is plain HTTP — anyone
> who obtains that URL, or who can watch the traffic, sees whatever the page
> shows, including logged-in content. Tunnel it or front it with TLS on an
> untrusted network. See [docs/security.md](docs/security.md).

`serve` accepts JSON-RPC methods including: `browser.open` (alias `page.goto`),
`observe`, `screenshot`, `click`, `hover`, `mouse.move`, `mouse.down`, `mouse.up`,
`scroll`, `mouse.drag`, `mouse.drag_path`, `type`, `insert-text`, `key.press`,
`key.down`, `key.up`, `wait`, `console`, `network`, `evaluate`, `url`, `title`,
`browser.close`, plus the experimental `dewiggle`, `trace.start`, `trace.stop`,
and `replay`. Add `--json` to launch/run for machine output (on `view`, the
`--json` viewer banner is printed to stderr — stdout stays protocol-only).

## Error model

Errors are structured so an agent can decide recovery. Example:

```json
{
  "id": 14,
  "error": {
    "code": "STALE_REFERENCE",
    "message": "stale reference @e3",
    "recovery": "Reference @e3 is stale. Run the `observe` method again and use the new reference."
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

Error codes: `BROWSER_NOT_FOUND`, `LAUNCH_FAILED`, `CONNECTION_FAILED`,
`PROTOCOL_ERROR`, `TIMEOUT`, `TARGET_CLOSED`, `ELEMENT_NOT_FOUND`,
`ELEMENT_NOT_INTERACTABLE`, `STALE_REFERENCE`, `INVALID_INPUT`,
`NAVIGATION_BLOCKED`, `NAVIGATION_FAILED`, `EVALUATION_FAILED`,
`UNEXPECTED_RESPONSE`, `TRACE_ERROR`, `DECODE_ERROR`, `IO_ERROR`,
`INTERNAL_ERROR`. See [docs/protocol.md](docs/protocol.md).

## Docker example

```bash
docker build -f docker/Dockerfile -t headless-use .
# -i keeps stdin open: serve reads JSON-RPC from stdin and exits on EOF.
docker run --rm -i --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  headless-use serve --no-sandbox
```

Or use the shipped [`docker/docker-compose.yml`](docker/docker-compose.yml).

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
- HTML5 file drop with MIME handling
- `install-browser` with checksum verification

## Security

See [docs/security.md](docs/security.md). Key points: CDP binds to `127.0.0.1`
only, secrets are masked in traces (including auto-detection of password fields),
agent-supplied file paths (`trace.start`, `replay`) are confined to the working
directory, Chrome site isolation stays on, and a host allow/deny policy is
enforced on navigation (`--allow-host`/`--deny-host`) — which also restricts
navigation to `http`/`https`, so `file:`/`data:`/`javascript:` URLs cannot slip
past a host rule. The live viewer is loopback-only unless you widen it with
`--viewer-host`, and a non-loopback bind requires the `?token=` access token
(`--viewer-token`); on loopback the token is accepted but not demanded.

## Community

- [Contributing](CONTRIBUTING.md) — development setup, code standards, PR process
- [Code of Conduct](CODE_OF_CONDUCT.md) — community standards
- [Security Policy](SECURITY.md) — vulnerability reporting
- [Changelog](CHANGELOG.md) — release history
- [Discussions](https://github.com/flyingsquirrel0419/headless-use/discussions) — questions & ideas

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
