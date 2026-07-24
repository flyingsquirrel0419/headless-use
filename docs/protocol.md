# JSON-RPC protocol

## Framing

Newline-delimited JSON-RPC 2.0 over stdio (one JSON object per line).

## Request

```json
{ "id": 14, "method": "mouse.click", "params": { "ref": "@e3" }, "jsonrpc": "2.0" }
```

## Success response

```json
{ "id": 14, "result": { "success": true }, "jsonrpc": "2.0" }
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
  "jsonrpc": "2.0"
}
```

## Protocol version

`jsonrpc: "2.0"`. The tool set is versioned via the crate version; new methods are
additive. Removed/renamed methods require a major version bump. See
`docs/architecture.md` for the method list.

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
| `IO_ERROR` | An I/O error occurred. |
| `INTERNAL_ERROR` | An opaque internal error. |

## Trace & replay methods

| Method | Params | Description |
| --- | --- | --- |
| `trace.start` | `{base?: string}` | Start recording; returns `{traceDir, started}`. |
| `trace.stop` | `{}` | Stop, flush, write report.html; returns `{traceDir, stopped}`. |
| `replay` | `{runDir: string}` | Replay a recorded trace; returns a `ReplayResult`. |

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
{"id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"headless-use","version":"0.1.0"}},"jsonrpc":"2.0"}
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
