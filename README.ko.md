# headless-use

[English](https://github.com/flyingsquirrel0419/headless-use/blob/main/README.md) | [한국어](https://github.com/flyingsquirrel0419/headless-use/blob/main/README.ko.md) | [日本語](https://github.com/flyingsquirrel0419/headless-use/blob/main/README.ja.md) | [中文](https://github.com/flyingsquirrel0419/headless-use/blob/main/README.zh.md)

<p align="center">
  <img src="https://raw.githubusercontent.com/flyingsquirrel0419/headless-use/main/docs/assets/demo.gif" alt="headless-use demo" width="720">
</p>


> 웹 개발 에이전트를 위한 컴퓨터 사용, 헤드리스 Linux와 CI에 최적화.

`headless-use`는 AI 코딩 에이전트가 직접 만든 웹앱을 **보고, 조작하고, 디버그**할
수 있게 해주는 경량 브라우저 런타임입니다. Chrome을 Chrome DevTools
Protocol(CDP)로 제어하며, JavaScript `element.click()`이 아닌 **실제 입력
이벤트**를 사용합니다. 에이전트에게 스크린샷 기반 "컴퓨터 사용"과 토큰 절약형
의미 참조(`@g1:e1`, `@g1:e2`, …; 축약형 `@eN`도 허용)를 모두 제공합니다.

Node.js 런타임이 없는 단일 Rust 바이너리로, GUI 없는 서버, Docker, CI에서
Xvfb 없이 동작하도록 설계되었습니다.

---

## 왜 필요한가

브라우저 자동화 도구는 **테스트 스크립트**를 위해 만들어졌습니다. 에이전트에게
필요한 것은 **실시간 상호작용**입니다: 페이지를 관찰하고, 다음 행동을 결정하고,
결과를 검증합니다. 기존 도구는 셀렉터 중심, DOM 전용이며, 매 단계마다
에이전트가 다시 스크린샷을 찍게 만듭니다. `headless-use`는 에이전트
중심입니다:

| 일반 브라우저 자동화                   | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| 테스트 코드 작성                       | 실시간 에이전트 조작                       |
| CSS 셀렉터                             | 좌표 **그리고** 의미 참조                  |
| DOM 전용                               | 스크린샷 + DOM + Console + Network         |
| Node.js 의존성이 흔함                  | 단일 Rust 바이너리                         |
| 로컬 데스크톱 중심                     | 헤드리스 Linux, Docker, CI 우선            |
| 결과만 제공                            | 세션 트레이스, 진단 리포트                 |

## 30초 Quick Start

```bash
# 빌드 (단일 바이너리, 런타임 의존성 없음)
cargo build --release

# 환경 진단
./target/release/headless-use doctor
```

`serve`는 stdin/stdout으로 newline-delimited JSON-RPC를 말하는 장수명 세션을
시작합니다(stdout에는 **프로토콜 응답만** 나오고, 로그와 배너는 stderr로
갑니다). 요청은 하나의 프로세스에 파이프로 넣으세요 — 각 `serve`가 자기
브라우저를 소유합니다:

```bash
# 페이지 열기, 인터랙티브 요소 관찰, 참조로 클릭, 타이핑.
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@g1:e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe`는 페이지 메타데이터와 인터랙티브 요소 목록을 반환합니다. 각 요소의
generation 결합 참조(`ref`)는 `click`, `hover`, `screenshot`의 대상으로 쓸 수
있습니다:

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

(요소에는 더 많은 필드가 있습니다 — bounding box, `visible`, `enabled`,
`checked`, `value`, `selectorHint` — 여기서는 생략했습니다. 축약형 `@eN`도
허용되지만, 내비게이션 이후 stale 참조를 감지하는 것은 전체 형식
`@g<gen>:eN`뿐입니다.)

기본 경로를 가볍게 유지하기 위해 두 가지 비용은 opt-in입니다:

- `observe`에 `"listeners": true`를 주면 CDP로 프로그램적으로 부착된 클릭
  리스너까지 감지합니다(후보 요소당 최대 2회의 추가 CDP 왕복).
- `click`에 `"effects": true`를 주면 클릭 후 300ms 동안 효과(`dom_mutations`,
  `network_requests`, `navigated`, `focus_changed`)를 샘플링합니다. 없으면
  `effects`는 `null`이고 저렴한 클릭 전 hit test만 실행됩니다.

## One-shot 모드

```bash
# URL을 열고 스크린샷을 저장한 뒤 종료.
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## 설치

### 배포 패키지

```bash
# Linux x86_64 바이너리 (호스트에 Chrome/Chromium 필요)
npm install --global headless-use

# crates.io 소스 빌드 설치 (호스트에 Chrome/Chromium 필요)
cargo install headless-use --locked

# Chromium 포함 컨테이너
docker pull ghcr.io/flyingsquirrel0419/headless-use:1.0.0
# 미러: docker pull flyingsquirrel0419/headless-use:1.0.0
```

미리 빌드된 압축 파일과 SHA-256 체크섬은
[GitHub Releases](https://github.com/flyingsquirrel0419/headless-use/releases)에
첨부됩니다. GitHub는 각 릴리스 태그의 전체 소스 zip과 tar.gz도 자동으로
제공합니다. v1 사전 빌드 패키지는 Linux x86_64만 지원합니다.

### 체크아웃한 소스에서

```bash
cargo install --path .
# PATH에 Chrome/Chromium이 필요하거나, HEADLESS_USE_BROWSER_PATH를 설정하세요.
```

### 브라우저 탐색

`headless-use`는 다음 순서로 브라우저를 찾습니다:

1. `HEADLESS_USE_BROWSER_PATH` 환경 변수
2. `PATH`의 `chrome-headless-shell`, `chromium-headless-shell`, `chromium`,
   `chromium-browser`, `google-chrome`, `google-chrome-stable`

`--stealth`에서는 순서가 뒤집힙니다: headless shell은 봇 체크가 읽는 브라우저
API가 빠져 있어 마지막에 시도됩니다([Stealth 모드](#stealth-모드-experimental)
참조).

명시적으로 지정:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker pull ghcr.io/flyingsquirrel0419/headless-use:1.0.0
# 이미지는 Chromium을 포함하며 non-root 사용자로, WORKDIR /home/hu에서
# 실행됩니다. 출력용 쓰기 가능 디렉터리를 마운트하세요 (먼저 mkdir -p output):
docker run --rm --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  -v "$PWD/output:/home/hu/output" \
  ghcr.io/flyingsquirrel0419/headless-use:1.0.0 \
  run --url http://127.0.0.1:3000 --screenshot output/page.png --no-sandbox
```

> **샌드박스 참고:** Chromium은 root로 실행 시 `--no-sandbox` 없이는 동작하지
> 않습니다. 프로세스가 root일 때(CI 컨테이너에서 흔함) `headless-use`가
> 자동으로 적용합니다. 제공되는 Dockerfile은 non-root 사용자로 실행되는데,
> 이 경우에도 Docker 기본 seccomp 프로필이 Chromium 샌드박스를 방해할 수
> 있습니다 — 그래서 위 예제는 제공되는 `docker/docker-compose.yml`과
> 동일하게 `--security-opt seccomp=unconfined` + `--no-sandbox`를 씁니다.
> 신뢰할 수 있는 프로덕션에서는 샌드박스를 켜두고 seccomp을 조정하는 쪽을
> 권장합니다. [SECURITY.md](https://github.com/flyingsquirrel0419/headless-use/blob/main/SECURITY.md) 참조.

## 안정성: v1이 보장하는 것

**core browser-control API는 v1.0.0부터 안정**이며 semver를 따릅니다:

- CLI 서브커맨드 `serve`, `run`, `mcp`, `launch`, `doctor`, `install-browser`
- JSON-RPC 메서드 `browser.open`/`page.goto`, `observe`, `click`, `hover`,
  `mouse.*`, `scroll`, `type`, `insert-text`, `key.*`, `wait`, `screenshot`,
  `console`, `network`, `evaluate`, `url`, `title`, `browser.close`
- 대응하는 MCP `browser_*` 도구
- 에러 모델(코드는 안정적인 문자열)과 응답 envelope
  (`jsonrpc`, `schemaVersion` — 결과 형태는 필드가 추가되기만 함)

**Experimental** — 지금도 사용 가능하지만, 형태와 기본값이 마이너 릴리스에서
바뀔 수 있고 v1 안정성 보장 대상이 아닙니다:

- **Live viewer** (`view`, `--viewer-*` 플래그, MJPEG 스트림)
- **Trace + Replay** (`trace.start`/`trace.stop`/`replay`, `actions.jsonl`,
  `report.html`)
- **Stealth 모드** (`--stealth`) — 핑거프린트 억제는 본질적으로 군비 경쟁
- **Dewiggle** (`dewiggle` 커맨드/메서드/도구)

## 지원 기능

- **실제 입력**: `Input.dispatchMouseEvent`, `dispatchKeyEvent`, `insertText`
- **마우스**: 이동, 클릭(left/right/middle/back/forward), 더블/트리플, down/up, hold, hover, 휠 스크롤, 드래그(보간), drag-path
- **키보드**: down/up/press, 코드(`Control+Shift+P`), type, insert-text(CJK/이모지 안전), hold, repeat
- **Observe**: DOM 기반 인터랙티브 요소 추출, 의미 참조 `@g<gen>:eN`(stale 감지를 위한 generation 결합), bounding box, 모든 내비게이션에서 stale 참조 감지; opt-in 리스너 스캔(`"listeners": true`)은 프로그램적으로 부착된 클릭 핸들러를 가진 요소를 승격시키고 `opaqueInteractive` 표면을 표시
- **클릭 리포트**: 모든 클릭이 클릭 전 hit test를 반환; opt-in(`"effects": true`) 클릭 후 효과 샘플링으로 dead click 감지
- **진단**: console + 미처리 에러, network(CDP `Network.*` 이벤트 — JS 몽키패칭 아님) + 시크릿 마스킹, wait-until-stable(활동 타임스탬프 기반, 폴링 사이 요청도 포착)
- **스크린샷**: viewport, full-page, 요소 영역(`--element @g1:e3`)
- **Dewiggle** *(experimental)*: 애니메이션 텍스트 CAPTCHA의 글자별 수직 흔들림을 **픽셀만으로** 복원 — 정답 배열, DOM 텍스트/props 안 읽음. N프레임 캡처, 각 열을 기준선으로 재정렬, 평균 내어 선명한 이미지 + 선택적 글자별 크롭 생성. `headless-use dewiggle --url ... --out out.png --chars 6`
- **Stealth** *(experimental)*: `--stealth`는 `--headless=new`를 유지하면서 스스로를 드러내지 않게 함 — [Stealth 모드](#stealth-모드-experimental) 참조
- **세션**: 장수명 `serve`(JSON-RPC stdio), one-shot `run`
- **Trace + Replay** *(experimental)*: `actions.jsonl`, `report.html`(자체 완결, 스크린샷 내장), 기록 경계에서의 강제 시크릿 마스킹, 기록된 트레이스를 재실행하는 `replay`
- **MCP 서버**: 스펙 준수 `initialize`/`tools/list`/`tools/call` over stdio

## MCP 서버

`headless-use mcp`는 stdio 위에서 스펙 준수 MCP 서버(protocolVersion
`2024-11-05`)를 실행합니다. AI 에이전트는 JSON-RPC 래핑 없이 바로 연결합니다:

```bash
headless-use mcp --no-sandbox
```

서버는 타입 있는 `inputSchema`를 가진 19개의 `browser_*` 도구를 광고합니다.
스크린샷 결과는 MCP 이미지 블록으로, 나머지는 압축된 JSON 텍스트 블록으로
반환됩니다. 에러는 `isError: true`와 복구 힌트를 반환합니다.

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

### 프로토콜 흐름

```
client → initialize {protocolVersion, capabilities}
server ← {protocolVersion, capabilities, serverInfo}
client → notifications/initialized
client → tools/list
server ← {tools: [...19 browser_* tools...]}
client → tools/call {name: "browser_observe", arguments: {}}
server ← {content: [{type:"text", text:"{...elements...}"}], isError: false}
```

## CLI 레퍼런스

```
headless-use
├── launch       브라우저를 실행하고 유지
├── serve        stdio 위 장수명 JSON-RPC 세션 시작
├── run          one-shot 액션 실행 후 종료
├── dewiggle     (experimental) 애니메이션 텍스트 영역 캡처 + 글자별 흔들림 복원 (픽셀만)
├── view         (experimental) 라이브 뷰어 + JSON-RPC 세션 (아래 참조)
├── replay       (experimental) run 디렉터리의 기록된 트레이스 재실행
├── doctor       환경 진단
├── install-browser   브라우저 설치 안내 출력
└── mcp          stdio 위 MCP 서버 시작
```

```bash
# 애니메이션 텍스트 CAPTCHA의 흔들림 복원, 글자별 크롭 6개 저장.
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### Stealth 모드 (experimental)

봇 체크(Cloudflare Turnstile 등) 뒤의 사이트는 headful Chrome은 그냥 통과하는
챌린지를 headless Chrome에게 들이댑니다. `--stealth`는 실제 디스플레이 비용
없이 그 간극을 좁힙니다 — `launch`, `serve`, `run`, `view`, `mcp`에서
동작합니다:

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

중요한 순서대로, 바꾸는 것들:

| 계층 | 제거되는 신호 |
| --- | --- |
| 실행 플래그 | `navigator.webdriver`(`--disable-blink-features=AutomationControlled` 사용), UA 문자열의 `HeadlessChrome/…` |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"` client-hint 헤더와 `navigator.userAgentData` — UA 플래그만으로는 **안** 고쳐짐 |
| 프리로드 스크립트 | SwiftShader WebGL 드라이버 문자열, 빈 `navigator.plugins`, 없는 `window.chrome`, `outerHeight == innerHeight`, 창 크기와 같은 screen, `notifications: denied` |
| Auto-attach | 챌린지 위젯이 실제로 도는 cross-origin iframe 내부에도 같은 처리 |

user agent는 브라우저 자신의 버전에서 파생되므로 UA, client-hint 브랜드 목록,
엔진이 모두 일치합니다. 이 도구 자신의 console/network 수집기를 포함한 모든
대체 함수는 native source(`function … () { [native code] }`)를 보고합니다 —
래핑된 `fetch`는 그 자체로 신호이기 때문입니다.

참고:

- Stealth는 `chrome-headless-shell`보다 **풀** Chrome/Chromium을 선호합니다.
  shell에는 `window.chrome`, PDF 플러그인 항목, 독점 코덱이 없고, 이것들은
  설득력 있게 위조할 수 없습니다. shell밖에 없으면 경고와 함께 사용합니다.
- `--stealth`에서 기본값 두 개가 바뀝니다: 스크롤바를 더 이상 숨기지 않고
  (폭 0 스크롤바는 문서화된 체크), WebGL이 존재하도록 GPU 프로세스를
  유지합니다(WebGL이 아예 없는 것이 소프트웨어 렌더러보다 더 큰 신호).
- 핑거프린트 억제는 군비 경쟁입니다. headless 브라우저를 즉시 식별 가능하게
  만드는 신호를 제거할 뿐, 모든 탐지기에 대한 보장은 아닙니다. 그래도
  챌린지가 나오면 `--compat xvfb`가 약 2배 메모리로 Xvfb 아래 진짜 headful
  브라우저를 실행합니다.

### Live viewer (experimental)

```bash
headless-use view --no-sandbox          # http://127.0.0.1:7780/?token=… 출력
```

`view`는 `serve`와 완전히 동일하게 동작하면서(JSON-RPC on stdio) 추가로
에이전트 커서 오버레이가 있는 페이지의 MJPEG 스트림을 제공합니다.

**액세스 토큰.** 매 실행마다 토큰이 생성되어 URL의 일부로 stderr에
출력됩니다(stdout은 JSON-RPC 채널) — 출력된 URL 그대로 여세요. URL이 고정돼야
하면 `--viewer-token <TOKEN>`으로 고정하세요. 토큰이 *강제*되는지는 바인드
주소에 따라 다릅니다:

| `--viewer-host` | 유효한 `?token=` 없는 요청 |
| --- | --- |
| loopback (`127.0.0.1`, 기본) | 제공됨 — 토큰은 허용되지만 선택 |
| 그 외 (`0.0.0.0`, LAN 주소) | `401 Unauthorized` |

```bash
headless-use view --viewer-host 0.0.0.0 --viewer-token "$(openssl rand -hex 16)"
```

**커서 모션.** `view`의 기본은 `--cursor-motion smooth`: 커서가 클릭/hover
대상까지 이동하며 실제 중간 `mouseMoved` 이벤트를 발생시킵니다. 스트림을
읽을 수 있게 만들고, 실제 움직임이 필요한 hover 메뉴도 열립니다. 클릭당 이동
시간(~220ms)이 들기 때문에 `serve`/`run`/`mcp`의 기본은 `instant`입니다.
어느 쪽으로든 재정의:

```bash
headless-use view  --cursor-motion instant   # 가장 빠름, 커서 순간이동
headless-use serve --cursor-motion smooth    # 느리지만 hover 메뉴 친화적
```

> **노출 참고:** 뷰어는 기본적으로 `127.0.0.1`에 바인드됩니다.
> `--viewer-host 0.0.0.0`은 네트워크에 여는 것이고, 그때 토큰이 필수입니다.
> 토큰은 URL 안의 bearer 자격 증명이므로 셸/브라우저 히스토리와 `Referer`
> 헤더에 남고, 스트림 자체는 평문 HTTP입니다 — 그 URL을 얻거나 트래픽을 볼
> 수 있는 누구든 로그인된 콘텐츠를 포함해 페이지가 보여주는 모든 것을
> 봅니다. 신뢰할 수 없는 네트워크에서는 터널링하거나 TLS를 앞에 두세요.
> [SECURITY.md](https://github.com/flyingsquirrel0419/headless-use/blob/main/SECURITY.md) 참조.

`serve`가 받는 JSON-RPC 메서드: `browser.open`(별칭 `page.goto`), `observe`,
`screenshot`, `click`, `hover`, `mouse.move`, `mouse.down`, `mouse.up`,
`scroll`, `mouse.drag`, `mouse.drag_path`, `type`, `insert-text`, `key.press`,
`key.down`, `key.up`, `wait`, `console`, `network`, `evaluate`, `url`,
`title`, `browser.close`, 그리고 experimental인 `dewiggle`, `trace.start`,
`trace.stop`, `replay`. launch/run에 `--json`을 붙이면 기계용 출력이 됩니다
(`view`의 `--json` 뷰어 배너는 stderr로 출력 — stdout은 프로토콜 전용 유지).

## 에러 모델

에이전트가 복구를 결정할 수 있도록 에러는 구조화되어 있습니다. 예:

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

에러 코드: `BROWSER_NOT_FOUND`, `LAUNCH_FAILED`, `CONNECTION_FAILED`,
`PROTOCOL_ERROR`, `TIMEOUT`, `TARGET_CLOSED`, `ELEMENT_NOT_FOUND`,
`ELEMENT_NOT_INTERACTABLE`, `STALE_REFERENCE`, `INVALID_INPUT`,
`NAVIGATION_BLOCKED`, `NAVIGATION_FAILED`, `EVALUATION_FAILED`,
`UNEXPECTED_RESPONSE`, `TRACE_ERROR`, `DECODE_ERROR`, `IO_ERROR`,
`INTERNAL_ERROR`. [docs/protocol.md](https://github.com/flyingsquirrel0419/headless-use/blob/main/docs/protocol.md) 참조.

## Docker 예제

```bash
docker build -f docker/Dockerfile -t headless-use .
# -i는 stdin을 열어둡니다: serve는 stdin에서 JSON-RPC를 읽고 EOF에서 종료.
docker run --rm -i --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  headless-use serve --no-sandbox
```

또는 제공되는 [`docker/docker-compose.yml`](https://github.com/flyingsquirrel0419/headless-use/blob/main/docker/docker-compose.yml)을
사용하세요.

## 제한사항

- cross-origin iframe 상호작용은 제한적이며, 문서화된 에러가 반환됩니다.
- OS 수준 IME 조합은 완전히 에뮬레이션되지 않음; CJK/이모지는 `Input.insertText` 사용.
- Firefox/WebKit은 초기 범위 밖 (Chromium 전용).
- 자동 브라우저 다운로드(`install-browser`)는 다운로드 대신 안내를 출력.
- 터치 입력(`touch tap`, `touch swipe`)은 구조적으로 지원되나 MVP CLI에는 없음.

## Playwright/Puppeteer와의 차이

`headless-use`는 Playwright를 감싸지 **않습니다**. WebSocket 위에서 CDP를 직접
말하고, 프로세스 수명주기(임시 프로필 정리, 좀비 방지, 시그널 처리)를 직접
소유하며, 에이전트 우선 API(참조 + observe + 구조화된 에러)를 노출합니다.
호환성 확인을 위해 Playwright와 병행 사용할 수 있으나, Playwright 래퍼가
아닙니다.

## 로드맵

- `report.html` 인터랙티브 타임라인
- MIME 처리를 포함한 HTML5 파일 드롭
- 체크섬 검증이 있는 `install-browser`

## 보안

[SECURITY.md](https://github.com/flyingsquirrel0419/headless-use/blob/main/SECURITY.md) 참조. 핵심: CDP는 `127.0.0.1`에만
바인드, 트레이스에서 시크릿 마스킹(비밀번호 필드 자동 감지 포함),
에이전트가 제공한 파일 경로(`trace.start`, `replay`)는 작업 디렉터리로 제한,
Chrome 사이트 격리 유지, 호스트 allow/deny 정책이 내비게이션에 강제됨
(`--allow-host`/`--deny-host`) — 이때 내비게이션은 `http`/`https`로도
제한되어 `file:`/`data:`/`javascript:` URL이 호스트 규칙을 우회할 수
없습니다. 라이브 뷰어는 `--viewer-host`로 넓히지 않는 한 loopback 전용이며,
non-loopback 바인드에는 `?token=` 액세스 토큰(`--viewer-token`)이 필수입니다.
loopback에서는 토큰이 허용되지만 요구되지 않습니다.

## 커뮤니티

- [Contributing](https://github.com/flyingsquirrel0419/headless-use/blob/main/CONTRIBUTING.md) — 개발 환경, 코드 표준, PR 프로세스
- [Release Guide](https://github.com/flyingsquirrel0419/headless-use/blob/main/docs/releasing.md) — 배포, Actions Secrets, 빌드 증명
- [Code of Conduct](https://github.com/flyingsquirrel0419/headless-use/blob/main/CODE_OF_CONDUCT.md) — 커뮤니티 표준
- [Security Policy](https://github.com/flyingsquirrel0419/headless-use/blob/main/SECURITY.md) — 취약점 보고
- [Changelog](https://github.com/flyingsquirrel0419/headless-use/blob/main/CHANGELOG.md) — 릴리스 이력
- [Discussions](https://github.com/flyingsquirrel0419/headless-use/discussions) — 질문과 아이디어

## 라이선스

Apache License, Version 2.0. [LICENSE](https://github.com/flyingsquirrel0419/headless-use/blob/main/LICENSE) 참조.
