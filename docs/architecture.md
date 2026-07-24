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
      ├─ Observation Engine (AX/DOM, references)
      ├─ Stability Detector (wait)
      ├─ Console/Network Collector
      ├─ Screenshot Engine
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

## Session lifecycle

`Session` owns a `Browser` + active `Page`. `serve` keeps it alive and dispatches
JSON-RPC; `run` creates one, performs one action, and tears down. Tracing wraps
every action into `actions.jsonl` + `report.html`.
