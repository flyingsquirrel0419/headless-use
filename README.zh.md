# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)


> 面向 Web 开发代理的计算机使用工具，专为无头 Linux 和 CI 环境构建。

`headless-use` 是一个轻量级浏览器运行时，让 AI 编码代理能够**查看、操作和调试**
它们构建的 Web 应用。它通过 Chrome DevTools Protocol（CDP）控制 Chrome，使用
**真实输入事件**而非 JavaScript 的 `element.click()`，并为代理同时提供基于截图
的"计算机使用"和节省 token 的语义引用（`@g1:e1`、`@g1:e2`、…；也接受简写
`@eN`）。

它是单个 Rust 二进制文件，无需 Node.js 运行时，专为在无 GUI 的服务器、Docker
和 CI 环境中运行而设计，无需 Xvfb。

---

## 为什么需要它

浏览器自动化工具是为**测试脚本**设计的。代理需要的是**实时交互**：观察页面、
决定下一步动作、验证结果。传统工具以选择器为中心、仅限 DOM，迫使代理每一步都
重新截图。`headless-use` 以代理为中心：

| 通用浏览器自动化                       | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| 编写测试代码                           | 实时代理操作                               |
| CSS 选择器                             | 坐标**和**语义引用                         |
| 仅 DOM                                 | 截图 + DOM + Console + Network             |
| 通常依赖 Node.js                       | 单个 Rust 二进制                           |
| 面向本地桌面                           | 无头 Linux、Docker、CI 优先                |
| 只有结果                               | 会话 trace、诊断报告                       |

## 30 秒快速开始

```bash
# 构建（单个二进制，无运行时依赖）
cargo build --release

# 诊断环境
./target/release/headless-use doctor
```

`serve` 启动一个长驻会话，在 stdin/stdout 上使用 newline-delimited JSON-RPC
（stdout **只**输出协议响应；日志和横幅走 stderr）。把请求通过管道送进同一个
进程 — 每个 `serve` 拥有自己的浏览器：

```bash
# 打开页面、观察交互元素、按引用点击、输入文本。
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@g1:e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe` 返回页面元数据和交互元素列表，每个元素带有 generation 绑定的引用
（`ref`），可作为 `click`、`hover`、`screenshot` 的目标：

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

（元素还包含更多字段 — bounding box、`visible`、`enabled`、`checked`、
`value`、`selectorHint` — 此处为简洁省略。简写 `@eN` 也被接受，但只有完整
形式 `@g<gen>:eN` 能在导航后检测 stale 引用。）

为保持默认路径轻量，两项开销为 opt-in：

- `observe` 加 `"listeners": true` 时，额外通过 CDP 检测以编程方式附加的点击
  监听器（每个候选元素最多 2 次额外 CDP 往返）。
- `click` 加 `"effects": true` 时，采样点击后 300ms 内的效果
  （`dom_mutations`、`network_requests`、`navigated`、`focus_changed`）；
  不加则 `effects` 为 `null`，只运行廉价的点击前 hit test。

## 单次模式

```bash
# 打开 URL 并保存截图，然后退出。
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## 安装

### 从源码

```bash
cargo install --path .
# 需要 PATH 上有 Chrome/Chromium，或设置 HEADLESS_USE_BROWSER_PATH。
```

### 浏览器发现

`headless-use` 按以下顺序查找浏览器：

1. `HEADLESS_USE_BROWSER_PATH` 环境变量
2. `PATH` 上的 `chrome-headless-shell`、`chromium-headless-shell`、
   `chromium`、`chromium-browser`、`google-chrome`、`google-chrome-stable`

使用 `--stealth` 时顺序反转：headless shell 缺少机器人检测会读取的浏览器
API，因此最后尝试（见[隐身模式](#隐身模式-experimental)）。

显式指定：

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -f docker/Dockerfile -t headless-use .
# 镜像捆绑 Chromium，以非 root 用户运行，WORKDIR 为 /home/hu。
# 挂载一个可写目录用于输出（先 mkdir -p output）：
docker run --rm --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  -v "$PWD/output:/home/hu/output" \
  headless-use \
  run --url http://127.0.0.1:3000 --screenshot output/page.png --no-sandbox
```

> **沙箱说明：** Chromium 以 root 运行时若无 `--no-sandbox` 会拒绝启动；当
> 进程为 root 时（CI 容器中常见），`headless-use` 会自动应用它。随附的
> Dockerfile 以非 root 用户运行，但 Docker 默认的 seccomp 配置仍可能干扰
> Chromium 沙箱 — 因此上面的示例与随附的 `docker/docker-compose.yml` 一致，
> 使用 `--security-opt seccomp=unconfined` 加 `--no-sandbox`。在可信的生产
> 环境中，建议保持沙箱开启并调整 seccomp。见
> [docs/security.md](docs/security.md)。

## 稳定性：v1 保证什么

**核心浏览器控制 API 自 v1.0.0 起稳定**，遵循 semver：

- CLI 子命令 `serve`、`run`、`mcp`、`launch`、`doctor`、`install-browser`
- JSON-RPC 方法 `browser.open`/`page.goto`、`observe`、`click`、`hover`、
  `mouse.*`、`scroll`、`type`、`insert-text`、`key.*`、`wait`、`screenshot`、
  `console`、`network`、`evaluate`、`url`、`title`、`browser.close`
- 对应的 MCP `browser_*` 工具
- 错误模型（错误码是稳定的字符串）与响应 envelope
  （`jsonrpc`、`schemaVersion` — 结果形状只增加字段）

**Experimental** — 现在可用，但形状和默认值可能在次版本中变化，不在 v1
稳定性保证范围内：

- **实时查看器**（`view`、`--viewer-*` 参数、MJPEG 流）
- **Trace + Replay**（`trace.start`/`trace.stop`/`replay`、`actions.jsonl`、
  `report.html`）
- **隐身模式**（`--stealth`）— 指纹抑制本质上是军备竞赛
- **Dewiggle**（`dewiggle` 命令/方法/工具）

## 支持的功能

- **真实输入**: `Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **鼠标**: 移动、点击（left/right/middle/back/forward）、双击/三击、down/up、hold、hover、滚轮滚动、拖拽（插值）、drag-path
- **键盘**: down/up/press、组合键（`Control+Shift+P`）、type、insert-text（CJK/emoji 安全）、hold、repeat
- **Observe**: 基于 DOM 的交互元素提取、语义引用 `@g<gen>:eN`（generation 绑定用于 stale 检测）、bounding box、任何导航后的 stale 引用检测；opt-in 监听器扫描（`"listeners": true`）提升带有编程附加点击处理器的元素，并标记 `opaqueInteractive` 表面
- **点击报告**: 每次点击返回点击前 hit test；opt-in（`"effects": true`）的点击后效果采样可检测无效点击
- **诊断**: console + 未捕获错误、network（CDP `Network.*` 事件 — 非 JS monkey-patch）+ 密钥脱敏、wait-until-stable（基于活动时间戳，可捕捉轮询间隙的请求）
- **截图**: viewport、整页、元素区域（`--element @g1:e3`）
- **Dewiggle** *(experimental)*: 仅用**像素**还原动画文字验证码的逐字符垂直抖动 — 不读取答案数组、DOM 文本/props。捕获 N 帧，将每列对齐到基线，平均后生成清晰图像及可选的逐字符裁剪。`headless-use dewiggle --url ... --out out.png --chars 6`
- **隐身** *(experimental)*: `--stealth` 保持 `--headless=new` 但不再自我暴露 — 见[隐身模式](#隐身模式-experimental)
- **会话**: 长驻 `serve`（JSON-RPC stdio）、单次 `run`
- **Trace + Replay** *(experimental)*: `actions.jsonl`、`report.html`（自包含，内嵌截图）、写入边界的强制密钥脱敏、重放已录制 trace 的 `replay`
- **MCP 服务器**: 符合规范的 `initialize`/`tools/list`/`tools/call` over stdio

## MCP 服务器

`headless-use mcp` 在 stdio 上运行符合规范的 MCP 服务器（protocolVersion
`2024-11-05`）。AI 代理无需包装 JSON-RPC 即可直接连接：

```bash
headless-use mcp --no-sandbox
```

服务器公布 19 个带类型化 `inputSchema` 的 `browser_*` 工具。截图结果以 MCP
图像块返回；其余以紧凑 JSON 文本块返回。错误返回 `isError: true` 和恢复提示。

### Claude Desktop / Cursor 配置

添加到 MCP 客户端配置：

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

### 协议流程

```
client → initialize {protocolVersion, capabilities}
server ← {protocolVersion, capabilities, serverInfo}
client → notifications/initialized
client → tools/list
server ← {tools: [...19 browser_* tools...]}
client → tools/call {name: "browser_observe", arguments: {}}
server ← {content: [{type:"text", text:"{...elements...}"}], isError: false}
```

## CLI 参考

```
headless-use
├── launch       启动浏览器并保持运行
├── serve        在 stdio 上启动长驻 JSON-RPC 会话
├── run          执行单次动作后退出
├── dewiggle     (experimental) 捕获动画文字区域并还原逐字符抖动（仅像素）
├── view         (experimental) 提供实时查看器 + JSON-RPC 会话（见下文）
├── replay       (experimental) 重放 run 目录中已录制的 trace
├── doctor       诊断环境
├── install-browser   打印浏览器安装指引
└── mcp          在 stdio 上启动 MCP 服务器
```

```bash
# 还原动画文字验证码的抖动，保存 6 个逐字符裁剪。
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### 隐身模式 (experimental)

处于机器人检测（Cloudflare Turnstile 等）之后的站点，会给 headless Chrome
出一个 headful Chrome 直接通过的挑战。`--stealth` 在不付出真实显示器成本的
情况下缩小这个差距 — 可用于 `launch`、`serve`、`run`、`view` 和 `mcp`：

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

按重要顺序，它改变的内容：

| 层 | 移除的信号 |
| --- | --- |
| 启动参数 | `navigator.webdriver`（通过 `--disable-blink-features=AutomationControlled`）、UA 字符串中的 `HeadlessChrome/…` |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"` client-hint 头和 `navigator.userAgentData` — 仅靠 UA 参数**修不好**这些 |
| 预加载脚本 | SwiftShader WebGL 驱动字符串、空的 `navigator.plugins`、缺失的 `window.chrome`、`outerHeight == innerHeight`、与窗口同大的 screen、`notifications: denied` |
| Auto-attach | 对 cross-origin iframe 内部做同样处理，那正是挑战组件运行指纹检测的地方 |

user agent 派生自浏览器自身的版本，因此 UA、client-hint 品牌列表与引擎完全
一致。所有替换函数都报告 native source（`function … () { [native code] }`），
包括本工具自己的 console/network 收集器 — 被包装的 `fetch` 本身就是信号。

说明：

- 隐身优先选择**完整**的 Chrome/Chromium 而非 `chrome-headless-shell`。shell
  没有 `window.chrome`、PDF 插件条目和专有编解码器；这些无法令人信服地伪造。
  若只有 shell 则带警告使用。
- `--stealth` 下有两个默认值改变：不再隐藏滚动条（零宽滚动条是已知检测点），
  并保持 GPU 进程存活以确保 WebGL 存在（完全没有 WebGL 比软件渲染器更响的
  信号）。
- 指纹抑制是军备竞赛。它移除让无头浏览器一眼可辨的信号，但不保证骗过所有
  检测器。若站点仍发起挑战，`--compat xvfb` 以约两倍内存在 Xvfb 下运行真正
  的 headful 浏览器。

### 实时查看器 (experimental)

```bash
headless-use view --no-sandbox          # 打印 http://127.0.0.1:7780/?token=…
```

`view` 的行为与 `serve` 完全相同（stdio 上的 JSON-RPC），并额外提供带代理
光标叠加层的页面 MJPEG 流。

**访问令牌。** 每次运行都会生成令牌，作为 URL 的一部分打印到 stderr（stdout
是 JSON-RPC 通道）— 按打印出的 URL 直接打开。需要 URL 保持稳定时用
`--viewer-token <TOKEN>` 固定。令牌是否*强制*取决于绑定地址：

| `--viewer-host` | 没有有效 `?token=` 的请求 |
| --- | --- |
| loopback（`127.0.0.1`，默认） | 提供服务 — 令牌被接受但可选 |
| 其他（`0.0.0.0`、LAN 地址） | `401 Unauthorized` |

```bash
headless-use view --viewer-host 0.0.0.0 --viewer-token "$(openssl rand -hex 16)"
```

**光标运动。** `view` 默认 `--cursor-motion smooth`：光标走向点击/hover
目标，发出真实的中间 `mouseMoved` 事件。这让流可读，也能触发需要真实移动的
hover 菜单。每次点击花费移动时间（~220ms），因此 `serve`/`run`/`mcp` 默认
`instant`。均可覆盖：

```bash
headless-use view  --cursor-motion instant   # 最快，光标瞬移
headless-use serve --cursor-motion smooth    # 较慢，hover 菜单友好
```

> **暴露说明：** 查看器默认绑定 `127.0.0.1`。`--viewer-host 0.0.0.0` 将其
> 开放到网络，此时令牌为必需。令牌是 URL 中的 bearer 凭证，会留在 shell 和
> 浏览器历史以及 `Referer` 头中，且流本身是明文 HTTP — 任何拿到该 URL 或能
> 监听流量的人都能看到页面显示的一切，包括已登录内容。在不可信网络上请使用
> 隧道或在前面加 TLS。见 [docs/security.md](docs/security.md)。

`serve` 接受的 JSON-RPC 方法包括：`browser.open`（别名 `page.goto`）、
`observe`、`screenshot`、`click`、`hover`、`mouse.move`、`mouse.down`、
`mouse.up`、`scroll`、`mouse.drag`、`mouse.drag_path`、`type`、
`insert-text`、`key.press`、`key.down`、`key.up`、`wait`、`console`、
`network`、`evaluate`、`url`、`title`、`browser.close`，以及 experimental 的
`dewiggle`、`trace.start`、`trace.stop`、`replay`。launch/run 加 `--json`
可获得机器可读输出（`view` 的 `--json` 查看器横幅打印到 stderr — stdout
保持仅协议）。

## 错误模型

错误是结构化的，便于代理决定如何恢复。示例：

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

错误码：`BROWSER_NOT_FOUND`、`LAUNCH_FAILED`、`CONNECTION_FAILED`、
`PROTOCOL_ERROR`、`TIMEOUT`、`TARGET_CLOSED`、`ELEMENT_NOT_FOUND`、
`ELEMENT_NOT_INTERACTABLE`、`STALE_REFERENCE`、`INVALID_INPUT`、
`NAVIGATION_BLOCKED`、`NAVIGATION_FAILED`、`EVALUATION_FAILED`、
`UNEXPECTED_RESPONSE`、`TRACE_ERROR`、`DECODE_ERROR`、`IO_ERROR`、
`INTERNAL_ERROR`。见 [docs/protocol.md](docs/protocol.md)。

## Docker 示例

```bash
docker build -f docker/Dockerfile -t headless-use .
# -i 保持 stdin 打开：serve 从 stdin 读取 JSON-RPC，EOF 时退出。
docker run --rm -i --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  headless-use serve --no-sandbox
```

或使用随附的 [`docker/docker-compose.yml`](docker/docker-compose.yml)。

## 限制

- cross-origin iframe 交互受限；返回已文档化的错误。
- 未完全模拟操作系统级 IME 组合；CJK/emoji 使用 `Input.insertText`。
- Firefox/WebKit 不在初始范围内（仅 Chromium）。
- 自动浏览器下载（`install-browser`）只打印指引而不下载。
- 触摸输入（`touch tap`、`touch swipe`）结构上支持但不在 MVP CLI 中。

## 与 Playwright/Puppeteer 的区别

`headless-use` **不**包装 Playwright。它直接在 WebSocket 上使用 CDP，自己
掌控进程生命周期（临时配置清理、僵尸进程预防、信号处理），并暴露代理优先的
API（引用 + observe + 结构化错误）。它可以与 Playwright 并行用于兼容性
检查，但不是 Playwright 的包装器。

## 路线图

- `report.html` 交互式时间线
- 带 MIME 处理的 HTML5 文件拖放
- 带校验和验证的 `install-browser`

## 安全

见 [docs/security.md](docs/security.md)。要点：CDP 仅绑定 `127.0.0.1`，trace
中脱敏密钥（包括密码字段自动检测），代理提供的文件路径（`trace.start`、
`replay`）被限制在工作目录内，Chrome 站点隔离保持开启，主机 allow/deny 策略
在导航时强制执行（`--allow-host`/`--deny-host`）— 同时导航被限制为
`http`/`https`，因此 `file:`/`data:`/`javascript:` URL 无法绕过主机规则。
实时查看器除非用 `--viewer-host` 放宽，否则仅限 loopback；非 loopback 绑定
需要 `?token=` 访问令牌（`--viewer-token`）；loopback 上令牌被接受但不强制。

## 社区

- [Contributing](CONTRIBUTING.md) — 开发环境、代码标准、PR 流程
- [Code of Conduct](CODE_OF_CONDUCT.md) — 社区标准
- [Security Policy](SECURITY.md) — 漏洞报告
- [Changelog](CHANGELOG.md) — 发布历史
- [Discussions](https://github.com/flyingsquirrel0419/headless-use/discussions) — 问题与想法

## 许可证

Apache License, Version 2.0。见 [LICENSE](LICENSE)。
