# Examples

## Login flow (JSON-RPC)

```bash
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000/login"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"click","params":{"ref":"@e2"},"jsonrpc":"2.0"}' \
  '{"id":6,"method":"insert-text","params":{"text":"비밀번호"},"jsonrpc":"2.0"}' \
  '{"id":7,"method":"key.press","params":{"chord":"Enter"},"jsonrpc":"2.0"}' \
  '{"id":8,"method":"wait","params":{"timeout":5000},"jsonrpc":"2.0"}' \
  '{"id":9,"method":"screenshot","params":{"fullPage":false},"jsonrpc":"2.0"}' \
  '{"id":10,"method":"console","params":{"level":"error"},"jsonrpc":"2.0"}' \
  '{"id":11,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | headless-use serve --no-sandbox
```

## Coordinate-based interaction

```bash
# All via JSON-RPC params:
# mouse.move      {"x":500,"y":300}
# mouse.click      {"x":500,"y":300,"button":"right","count":2}
# scroll           {"dx":0,"dy":600,"steps":20,"duration":500}
# mouse.drag       {"from":[200,400],"to":[800,400],"steps":35,"duration":700}
# mouse.drag_path  {"path":"[[100,200],[400,350],[700,500]]"}
```

## Trace + report

`serve` records a trace when the session is created with tracing (programmatic
API). Artifacts land in `.headless-use/runs/<timestamp>-<id>/`:
`actions.jsonl`, `metadata.json`, `report.html`, `screenshots/`.
