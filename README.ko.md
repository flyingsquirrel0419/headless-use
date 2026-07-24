# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)

> 웹 개발 에이전트를 위한 컴퓨터 사용, 헤드리스 Linux와 CI에 최적화.

`headless-use`는 AI 코딩 에이전트가 직접 만든 웹앱을 **보고, 조작하고, 디버그**할 수 있게 해주는 경량 브라우저 런타임입니다. Chrome을 Chrome DevTools Protocol(CDP)로 제어하며, JavaScript `element.click()`이 아닌 **실제 입력 이벤트**를 사용합니다. 에이전트에게 스크린샷 기반 "컴퓨터 사용"과 토큰 절약형 의미 참조(`@e1`, `@e2`, …)를 모두 제공합니다.

Node.js 런타임 없이 단일 Rust 바이너리로 동작하며, Xvfb 없이 GUI가 없는 서버, Docker, CI 환경에서 실행되도록 설계되었습니다.

---

## 왜 필요한가

기존 브라우저 자동화 도구는 **테스트 스크립트 작성**에 맞춰져 있습니다. 에이전트는 **실시간 상호작용**이 필요합니다: 페이지를 관찰하고, 다음 동작을 결정하고, 결과를 검증해야 합니다. `headless-use`는 에이전트 중심입니다:

| 일반 브라우저 자동화            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| 테스트 코드 작성                     | 실시간 에이전트 조작                 |
| CSS 셀렉터                         | 좌표 **와** 의미 참조  |
| DOM만                              | 스크린샷 + AX/DOM + 콘솔 + 네트워크   |
| Node.js 의존성 흔함             | 단일 Rust 바이너리                        |
| 로컬 데스크톱 중심                 | 헤드리스 Linux, Docker, CI 우선          |
| 결과만 제공                           | 세션 트레이스, 재현, 진단 보고서 |

## 30초 퀵스타트

```bash
# 빌드 (단일 바이너리, 런타임 의존성 없음)
cargo build --release

# 환경 진단
./target/release/headless-use doctor

# 장기 세션 시작 (stdio 기반 JSON-RPC)
./target/release/headless-use serve --no-sandbox
```

다른 터미널에서 실행 중인 세션에 JSON-RPC 요청을 보냅니다:

```bash
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe`는 상호작용 가능한 요소 목록을 안정적인 참조와 함께 반환합니다:

```
[@e1] textbox "이메일"
[@e2] textbox "비밀번호"
[@e3] button "로그인"
[@e4] link "회원가입"
[@e5] checkbox "" [unchecked]
```

## 원샷 모드

```bash
# URL을 열고 스크린샷을 저장한 후 종료
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## 설치

### 소스에서 빌드

```bash
cargo install --path .
# PATH에 Chrome/Chromium이 필요, 또는 HEADLESS_USE_BROWSER_PATH 설정
```

### 브라우저 탐색

`headless-use`는 다음 순서로 브라우저를 찾습니다:

1. `HEADLESS_USE_BROWSER_PATH` 환경변수
2. `PATH`의 `chrome-headless-shell`, `chromium`, `google-chrome`, `google-chrome-stable`

명시적으로 지정:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -t headless-use .
# 이미지에 Chromium이 포함됨. 원샷 실행:
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **샌드박스 참고:** Chromium은 root로 실행 시 `--no-sandbox` 없이 동작하지 않습니다. Docker CI에서는 격리된 빌드이므로 허용됩니다. 신뢰할 수 있는 프로덕션에서는 non-root 사용자로 실행(제공되는 Dockerfile이 그렇게 함)하고 샌드박스를 켜두세요. [docs/security.md](docs/security.md) 참조.

## 지원 기능

- **실제 입력**: `Input.dispatchMouseEvent`, `dispatchKeyEvent`, `insertText`
- **마우스**: 이동, 클릭(좌/우/중/뒤/앞), 더블/트리플, down/up, hold, hover, 휠 스크롤, 드래그(보간), drag-path
- **키보드**: down/up/press, 조합키(`Control+Shift+P`), type, insert-text(CJK/이모지 안전), hold, repeat
- **관찰**: 접근성/DOM 추출, 의미 `@eN` 참조, 바운딩 박스, stale 참조 감지
- **진단**: 콘솔 + 미잡 에러, 네트워크(fetch/XHR) 비밀 마스킹, wait-until-stable
- **스크린샷**: 뷰포트, 전체 페이지, 요소
- **세션**: 장기 `serve`(JSON-RPC stdio), 원샷 `run`, 트레이스 + 재현
- **트레이스**: `actions.jsonl`, 스크린샷, `report.html`(독립형), 비밀 자동 마스킹
- **MCP 서버**: stdio 기반 사양 준수 `initialize`/`tools/list`/`tools/call`

## MCP 서버

`headless-use mcp`는 stdio 기반으로 사양 준수 MCP 서버(protocolVersion `2024-11-05`)를 실행합니다. AI 에이전트가 JSON-RPC를 래핑 없이 직접 연결합니다:

```bash
headless-use mcp --no-sandbox
```

서버는 18개의 `browser_*` 도구를 타입이 지정된 `inputSchema`와 함께 제공합니다. 스크린샷 결과는 MCP 이미지 블록으로, 나머지는 간결한 JSON 텍스트 블록으로 반환됩니다. 에러는 `isError: true`와 복구 힌트를 반환합니다.

### Claude Desktop / Cursor 설정

MCP 클라이언트 설정에 추가:

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

## CLI 참조

```
headless-use
├── launch      브라우저를 실행하고 유지
├── serve        stdio 기반 장기 JSON-RPC 세션 시작
├── run          원샷 동작 실행 후 종료
├── doctor       환경 진단
├── install-browser   브라우저 설치 가이드 출력
└── mcp          stdio 기반 MCP 서버 시작
```

## 에러 모델

에러는 에이전트가 복구를 결정할 수 있도록 구조화됩니다:

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

에러 코드: `BROWSER_NOT_FOUND`, `LAUNCH_FAILED`, `CONNECTION_FAILED`, `PROTOCOL_ERROR`, `TIMEOUT`, `TARGET_CLOSED`, `ELEMENT_NOT_FOUND`, `ELEMENT_NOT_INTERACTABLE`, `STALE_REFERENCE`, `INVALID_INPUT`.

## 제한사항

- 크로스 오리진 iframe 조작에 제한이 있으며, 문서화된 에러를 반환합니다.
- OS 수준 IME 조합은 완전히 에뮬레이션되지 않습니다. CJK/이모지는 `Input.insertText`를 사용합니다.
- Firefox/WebKit은 초기 범위에서 제외됩니다(Chromium 전용).
- 자동 브라우저 다운로드(`install-browser`)는 다운로드 대신 가이드를 출력합니다.
- 터치 입력(`touch tap`, `touch swipe`)은 구조적으로 지원되지만 MVP CLI에는 포함되지 않습니다.

## 커뮤니티

- [기여 가이드](CONTRIBUTING.md) — 개발 설정, 코드 표준, PR 프로세스
- [행동 강령](CODE_OF_CONDUCT.md) — 커뮤니티 표준
- [보안 정책](SECURITY.md) — 취약점 신고
- [변경 이력](CHANGELOG.md) — 릴리스 내역
- [디스커션](https://github.com/headless-use/headless-use/discussions) — 질문 & 아이디어

## 라이선스

Apache License, Version 2.0에 따라 라이선스됩니다. [LICENSE](LICENSE) 참조.
