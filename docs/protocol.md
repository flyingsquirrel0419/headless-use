# JSON-RPC protocol

## Framing

Newline-delimited JSON-RPC 2.0 over stdio (one JSON object per line).

## Request

```json
{ "id": 14, "method": "mouse.click", "params": { "ref": "@e3" }, "jsonrpc": "2.0" }
```

## Success response

```json
{ "id": 14, "result": { "success": true }, "jsonrpc": "2.0", "schemaVersion": 1 }
```

## Error response

```json
{
  "id": 14,
  "error": {
    "code": "STALE_REFERENCE",
    "message": "stale reference @e3",
    "recovery": "Run observe again and use the new reference."
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

## Click result

`click` (MCP: `browser_click`) returns a report alongside `success`. The
pre-click hit test always runs; post-click effect sampling is **opt-in** via
`"effects": true` in the params (it costs three extra `Runtime.evaluate`
round-trips plus a 300 ms wait):

```json
{
  "id": 14,
  "result": {
    "success": true,
    "hit": { "element": "td#c3", "matched_target": true },
    "effects": { "dom_mutations": 2, "network_requests": 0, "navigated": false, "focus_changed": false }
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

- `hit` — pre-click `elementFromPoint` probe. `element` is a compact
  descriptor of what the click point lands on (or `null` if nothing).
  For ref clicks with a selector hint, `matched_target` says whether that is
  the addressed element; when it is not, `occluded_by` names the intercepting
  element. The click dispatches regardless — the report is advisory.
  `hit` is `null` when the probe itself failed.
- `effects` — what observably followed within the post-click window
  (300 ms): `dom_mutations`, `network_requests`, `navigated`,
  `focus_changed`. All zero/false means a **dead click** — nothing observable
  happened. `effects` is `null` unless requested with `"effects": true`
  (library callers: `Session::click_with_effects_window` or
  `Session::with_click_observe_window`).

## Observe params & result flags

Params (all optional):

- `mode` — `"interactive-only"` excludes visual widgets (canvas, svg,
  cursor:pointer divs).
- `listeners` — `true` additionally runs the **opt-in** listener scan
  (`DOMDebugger.getEventListeners` per candidate, up to 150 candidates and two
  CDP round-trips each). It promotes elements with programmatically-attached
  click listeners and flags delegation containers/canvas as
  `opaqueInteractive`. Off by default to keep observe light.

Result flags:

- `schemaVersion` — the result-schema version (see below).
- `truncated` — present (as `true`) at the top level of the `observe` result
  when the listener-detection scan hit its candidate cap; some clickable
  elements may be missing from the list. Omitted otherwise (and never present
  without `"listeners": true`).
- `opaqueInteractive` — per-element flag for interactive surfaces whose
  interior targets cannot be enumerated (event-delegation containers over
  inert children, canvas). Pick click coordinates from a screenshot instead
  of expecting child refs. Only produced by the listener scan.

## Protocol & schema version

`jsonrpc: "2.0"` is the envelope version. In addition, every response carries
an explicit `schemaVersion` (integer, currently `1`): the version of
headless-use's result/error shapes. The `observe` result repeats it inline,
and trace `metadata.json` carries it as `formatVersion`. It is bumped only
when an existing field changes meaning or is removed; purely additive changes
do not bump it. The tool set itself is versioned via the crate version; new
methods are additive, and removed/renamed methods require a major version
bump. See `docs/architecture.md` for the method list.

## Error codes

Stable machine-readable codes returned in the `error.code` field:

| Code | Meaning |
| --- | --- |
| `BROWSER_NOT_FOUND` | No browser executable on PATH or at the configured path. |
| `LAUNCH_FAILED` | The browser process failed to start. |
| `CONNECTION_FAILED` | Could not connect to the CDP WebSocket/HTTP endpoint. |
| `PROTOCOL_ERROR` | A CDP method returned a protocol-level error. |
| `TIMEOUT` | An operation did not complete within the timeout. |
| `TARGET_CLOSED` | The page/target closed unexpectedly. |
| `ELEMENT_NOT_FOUND` | A semantic reference did not resolve to any element. |
| `ELEMENT_NOT_INTERACTABLE` | An element exists but cannot be interacted with. |
| `STALE_REFERENCE` | A reference belongs to an older observe/navigation generation. |
| `INVALID_INPUT` | Invalid input (bad coordinate, unknown button, malformed path). |
| `NAVIGATION_BLOCKED` | Navigation blocked by the host allow/deny policy. |
| `NAVIGATION_FAILED` | Navigation to a URL failed. |
| `EVALUATION_FAILED` | A page script raised during `evaluate`. |
| `UNEXPECTED_RESPONSE` | The browser answered in an unexpected shape. |
| `TRACE_ERROR` | Trace recording failed (possibly incomplete trace). |
| `DECODE_ERROR` | Returned data could not be parsed. |
| `IO_ERROR` | An I/O error occurred. |
| `INTERNAL_ERROR` | An opaque internal error. |

## Trace & replay methods (experimental)

These methods are usable but **experimental**: shapes and defaults may change
in minor releases (see the Stability section in README.md).

| Method | Params | Description |
| --- | --- | --- |
| `trace.start` | `{base?: string}` | Start recording; returns `{traceDir, started}`. |
| `trace.stop` | `{}` | Stop, flush, write report.html; returns `{traceDir, stopped}`. |
| `replay` | `{runDir: string}` | Replay a recorded trace; returns a `ReplayResult`. |

Trace `metadata.json` carries `formatVersion` (the trace-format schema
version, currently `1`) plus the writing crate's `version`.

The `screenshot` method also accepts an optional `element` parameter
(`@g<gen>:e<num>`) to capture only that element's region.

## Backward compatibility

- New optional params are always additive.
- Result shapes only gain fields, never drop them.
- Error codes are stable strings; new codes may be added, existing ones keep meaning.

## MCP transport

The `mcp` subcommand implements the Model Context Protocol (protocolVersion
`2024-11-05`) over stdio. It is a separate transport from the plain JSON-RPC
`serve` because MCP requires:

1. An `initialize` → `notifications/initialized` handshake before tool calls.
2. Notifications (no `id`) which the plain `Request` parser rejects.
3. Tool results wrapped in `content` blocks (`{type, text}` or `{type, data,
   mimeType}` for images), not bare JSON-RPC results.

### `initialize` request/response

```json
// →
{"id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}},"jsonrpc":"2.0"}
// ←
{"id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"headless-use","version":"1.0.0"}},"jsonrpc":"2.0"}
```

### `tools/call`

```json
// →
{"id":3,"method":"tools/call","params":{"name":"browser_click","arguments":{"ref":"@e1"}},"jsonrpc":"2.0"}
// ←
{"id":3,"result":{"content":[{"type":"text","text":"{\"success\":true}"}],"isError":false},"jsonrpc":"2.0"}
```

Screenshots return an image content block:

```json
{"id":4,"result":{"content":[{"type":"image","data":"<base64>","mimeType":"image/png"}],"isError":false}}
```

Errors return `isError: true`:

```json
{"id":5,"result":{"content":[{"type":"text","text":"STALE_REFERENCE: stale reference @e3\nRecovery: ..."}],"isError":true}}
```

Calling `tools/list` or `tools/call` before `notifications/initialized` returns
JSON-RPC error code `-32002` ("Server not initialized").
