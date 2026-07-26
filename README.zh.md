# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)

> 面向 Web 开发代理的计算机使用工具，专为无头 Linux 和 CI 环境构建。

`headless-use` 是一个轻量级浏览器运行时，让 AI 编码代理能够**查看、操作和调试**它们构建的 Web 应用。它通过 Chrome DevTools Protocol（CDP）控制 Chrome，使用**真实输入事件**而非 JavaScript 的 `element.click()`，并为代理同时提供基于截图的"计算机使用"和节省 token 的语义引用（`@e1`、`@e2`、…）。

它是单个 Rust 二进制文件，无需 Node.js 运行时，专为在无 GUI 的服务器、Docker 和 CI 环境中运行而设计，无需 Xvfb。

---

## 为什么需要它

现有的浏览器自动化工具是为**编写测试脚本**而设计的。代理需要**实时交互**：观察页面、决定下一步操作、验证结果。传统工具以选择器为中心、只看 DOM，并且把每一步的重新截图都留给代理自己处理。`headless-use` 以代理为中心：

| 通用浏览器自动化            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| 编写测试代码                     | 实时代理操作                 |
| CSS 选择器                         | 坐标**和**语义引用  |
| 仅 DOM                              | 截图 + DOM + 控制台 + 网络   |
| 常见 Node.js 依赖             | 单个 Rust 二进制文件                        |
| 本地桌面为中心                 | 无头 Linux、Docker、CI 优先          |
| 仅结果                           | 会话追踪、诊断报告 |

## 30 秒快速开始

```bash
# 构建（单个二进制文件，无运行时依赖）
cargo build --release

# 诊断环境
./target/release/headless-use doctor

# 启动长期会话（基于 stdio 的 JSON-RPC）
./target/release/headless-use serve --no-sandbox
```

在另一个终端中，向运行中的会话发送 JSON-RPC 请求：

```bash
# 打开页面、观察可交互元素、按引用点击并输入。
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe` 返回可交互元素的简洁列表及稳定引用：

```
[@e1] textbox "邮箱"
[@e2] textbox "密码"
[@e3] button "登录"
[@e4] link "注册"
[@e5] checkbox "" [unchecked]
```

## 单次模式

```bash
# 打开 URL 并保存截图，然后退出
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## 安装

### 从源码构建

```bash
cargo install --path .
# 需要 PATH 上有 Chrome/Chromium，或设置 HEADLESS_USE_BROWSER_PATH
```

### 浏览器发现

`headless-use` 按以下顺序查找浏览器：

1. `HEADLESS_USE_BROWSER_PATH` 环境变量
2. `PATH` 上的 `chrome-headless-shell`、`chromium`、`google-chrome`、`google-chrome-stable`

使用 `--stealth` 时顺序相反：headless shell 会被放到最后，因为它们缺少机器人检测所读取的浏览器 API（参见[隐身模式](#隐身模式)）。

显式指定：

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -f docker/Dockerfile -t headless-use .
# 镜像内置 Chromium。单次运行：
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **沙箱注意：** Chromium 在 root 下运行时需要 `--no-sandbox`。在 Docker CI 中，这是隔离构建的可接受做法；对于可信的生产环境，建议以非 root 用户运行（附带的 Dockerfile 已这样做）并保持沙箱启用。参见 [docs/security.md](docs/security.md)。

## 支持的功能

- **真实输入**：`Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **鼠标**：移动、点击（左/右/中/后退/前进）、双击/三击、down/up、hold、hover、滚轮滚动、拖拽（插值）、drag-path
- **键盘**：down/up/press、组合键（`Control+Shift+P`）、type、insert-text（CJK/表情符号安全）、hold、repeat
- **观察**：基于 DOM 的可交互元素提取、语义 `@g<gen>:eN` 引用（绑定到观察代数，便于检测失效）、边界框、任意导航后的 stale 引用检测
- **诊断**：控制台 + 未捕获错误、网络（CDP `Network.*` 事件——不是 JS 猴子补丁）及密钥屏蔽、wait-until-stable（基于活动时间戳，可捕获两次轮询之间完成的请求）
- **截图**：视口、整页、元素区域（`--element @eN`）
- **Dewiggle**：**仅用像素**还原动画文字验证码中逐字符的上下抖动——不读取答案数组，也不读取 DOM 文本/属性。采集 N 帧，将每一列重新对齐到其中性基线，再平均为一张更锐利的图像，并可选输出逐字符切图。`headless-use dewiggle --url ... --out out.png --chars 6`
- **隐身**：`--stealth` 保持 `--headless=new`，但不再让浏览器自报无头身份——参见[隐身模式](#隐身模式)
- **会话**：长期 `serve`（JSON-RPC stdio）、单次 `run`、追踪 + 报告
- **追踪 + 回放**：`actions.jsonl`、`report.html`（自包含，内嵌截图）、在写入边界强制脱敏，以及用于重放已录制追踪的 `replay`
- **MCP 服务器**：基于 stdio 的规范兼容 `initialize`/`tools/list`/`tools/call`

## MCP 服务器

`headless-use mcp` 通过 stdio 运行规范兼容的 MCP 服务器（protocolVersion `2024-11-05`）。AI 代理无需包装 JSON-RPC 即可直接连接：

```bash
headless-use mcp --no-sandbox
```

服务器提供 19 个带有类型化 `inputSchema` 的 `browser_*` 工具。截图结果以 MCP 图像块返回，其他内容以简洁的 JSON 文本块返回。错误返回 `isError: true` 及恢复提示。

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
├── serve        启动基于 stdio 的长期 JSON-RPC 会话
├── run          执行单次操作并退出
├── dewiggle     采集动画文字区域并还原逐字符抖动（仅用像素）
├── view         提供实时查看器 + JSON-RPC 会话（见下文）
├── replay       重放运行目录中已录制的追踪
├── doctor       诊断环境
├── install-browser   显示浏览器安装指南
└── mcp          启动基于 stdio 的 MCP 服务器
```

```bash
# 还原动画文字验证码的抖动，并保存 6 张逐字符切图。
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### 隐身模式

带机器人检测（Cloudflare Turnstile 之类）的站点会给无头 Chrome 抛出挑战，而有头 Chrome 可以直接通过。`--stealth` 在不付出真实显示器代价的前提下弥合这一差距；它适用于 `launch`、`serve`、`run`、`view` 和 `mcp`：

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

按重要性排列，它改变了什么：

| 层次 | 消除的信号 |
| --- | --- |
| 启动参数 | `navigator.webdriver`（通过 `--disable-blink-features=AutomationControlled`）、UA 字符串中的 `HeadlessChrome/…` |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"` 客户端提示头与 `navigator.userAgentData` —— 仅靠 UA 参数**无法**修正这些 |
| 预加载脚本 | SwiftShader 的 WebGL 驱动字符串、空的 `navigator.plugins`、缺失的 `window.chrome`、`outerHeight == innerHeight`、与窗口同尺寸的屏幕、`notifications: denied` |
| 自动 attach | 对跨域 iframe 内部做同样处理，而挑战组件正是运行在那里 |

User agent 由浏览器自身的版本推导而来，因此 UA、客户端提示的品牌列表与引擎版本三者一致。所有被替换的函数都会报告原生源码（`function … () { [native code] }`），包括本工具自身的控制台/网络收集器——被包装过的 `fetch` 本身就是一个信号。

注意：

- 隐身模式优先选择**完整**的 Chrome/Chromium 而非 `chrome-headless-shell`。shell 构建没有 `window.chrome`、没有 PDF 插件条目、没有专有编解码器，这些无法令人信服地伪造。若只找得到 shell，则在告警后继续使用它。
- `--stealth` 会改变两个默认值：不再隐藏滚动条（零宽滚动条是已知的检测项），并保留 GPU 进程以便 WebGL 存在（完全没有 WebGL 比软件渲染器更显眼）。
- 指纹抑制是一场军备竞赛。它消除的是让无头浏览器被轻易识别的信号，并不保证能规避所有检测。如果站点仍然向你抛出挑战，`--compat xvfb` 会在 Xvfb 下运行真正的有头浏览器（内存开销约为两倍）。

### 实时查看器

```bash
headless-use view --no-sandbox          # http://127.0.0.1:7780/
```

`view` 的行为与 `serve` 完全一致（stdio 上的 JSON-RPC），并额外提供带有代理光标叠加层的页面 MJPEG 流。

**光标移动。** `view` 默认使用 `--cursor-motion smooth`：光标会走向点击/悬停目标，并发出真实的中间 `mouseMoved` 事件。这既让视频流易于观看，也能驱动那些需要真实移动的悬停菜单。它的代价是每次点击增加移动时间（约 220ms），因此 `serve`/`run`/`mcp` 默认为 `instant`。两个方向都可以覆盖：

```bash
headless-use view  --cursor-motion instant   # 最快，光标瞬移
headless-use serve --cursor-motion smooth    # 较慢，对悬停菜单友好
```

> **暴露风险提示：** 查看器默认绑定到 `127.0.0.1`。`--viewer-host 0.0.0.0` 会将其开放到网络，而该视频流**没有认证**——任何能访问该地址的人都能看到页面上显示的一切，包括已登录的内容。参见 [docs/security.md](docs/security.md)。

`serve` 接受的 JSON-RPC 方法包括：`browser.open`、`observe`、`screenshot`、`click`、`hover`、`mouse.move`、`mouse.down`、`mouse.up`、`scroll`、`mouse.drag`、`mouse.drag_path`、`type`、`insert-text`、`dewiggle`、`key.press`、`key.down`、`key.up`、`wait`、`console`、`network`、`browser.close`。为 launch/run 添加 `--json` 可获得机器可读输出。

## 错误模型

错误经过结构化处理，使代理能够决定恢复方式：

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

错误代码：`BROWSER_NOT_FOUND`、`LAUNCH_FAILED`、`CONNECTION_FAILED`、`PROTOCOL_ERROR`、`TIMEOUT`、`TARGET_CLOSED`、`ELEMENT_NOT_FOUND`、`ELEMENT_NOT_INTERACTABLE`、`STALE_REFERENCE`、`INVALID_INPUT`。

## Docker 示例

```bash
docker build -f docker/Dockerfile -t headless-use .
docker run --rm --network host --shm-size=1g headless-use \
  serve --no-sandbox
```

## 限制

- 跨域 iframe 操作有限；返回已记录的错误。
- OS 级 IME 输入未完全模拟；CJK/表情符号使用 `Input.insertText`。
- Firefox/WebKit 不在初始范围内（仅 Chromium）。
- 自动浏览器下载（`install-browser`）显示指南而非下载。
- 触摸输入（`touch tap`、`touch swipe`）在结构上受支持，但不在 MVP CLI 中。

## 与 Playwright/Puppeteer 的区别

`headless-use` **不**包装 Playwright。它通过 WebSocket 直接使用 CDP，自行掌控进程生命周期（临时配置目录清理、避免僵尸进程、信号处理），并提供代理优先的 API（引用 + observe + 结构化错误）。它可以与 Playwright 一起用于兼容性检查，但不是 Playwright 的封装。

## 路线图

- `report.html` 交互式时间线
- 带 MIME 处理的 HTML5 文件拖放
- 带校验和验证的 `install-browser`

## 安全

参见 [docs/security.md](docs/security.md)。要点：CDP 仅绑定 `127.0.0.1`；追踪中的密钥会被屏蔽（包括自动识别密码输入框）；代理提供的文件路径（`trace.start`、`replay`）被限制在工作目录内；Chrome 站点隔离保持开启；导航受主机允许/拒绝策略（`--allow-host`/`--deny-host`）约束——该策略同时将导航限制为 `http`/`https`，因此 `file:`/`data:`/`javascript:` 之类的 URL 无法绕过主机规则。实时查看器在未用 `--viewer-host` 放宽前仅监听回环地址，放宽后按设计不做认证。

## 社区

- [贡献指南](CONTRIBUTING.md) — 开发设置、代码标准、PR 流程
- [行为准则](CODE_OF_CONDUCT.md) — 社区标准
- [安全策略](SECURITY.md) — 漏洞报告
- [更新日志](CHANGELOG.md) — 发布历史
- [讨论](https://github.com/flyingsquirrel0419/headless-use/discussions) — 问题 & 想法

## 许可证

根据 Apache License, Version 2.0 授权。参见 [LICENSE](LICENSE)。
