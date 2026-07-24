# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)

> 面向 Web 开发代理的计算机使用工具，专为无头 Linux 和 CI 环境构建。

`headless-use` 是一个轻量级浏览器运行时，让 AI 编码代理能够**查看、操作和调试**它们构建的 Web 应用。它通过 Chrome DevTools Protocol（CDP）控制 Chrome，使用**真实输入事件**而非 JavaScript 的 `element.click()`，并为代理同时提供基于截图的"计算机使用"和节省 token 的语义引用（`@e1`、`@e2`、…）。

它是单个 Rust 二进制文件，无需 Node.js 运行时，专为在无 GUI 的服务器、Docker 和 CI 环境中运行而设计，无需 Xvfb。

---

## 为什么需要它

现有的浏览器自动化工具是为**编写测试脚本**而设计的。代理需要**实时交互**：观察页面、决定下一步操作、验证结果。`headless-use` 以代理为中心：

| 通用浏览器自动化            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| 编写测试代码                     | 实时代理操作                 |
| CSS 选择器                         | 坐标**和**语义引用  |
| 仅 DOM                              | 截图 + AX/DOM + 控制台 + 网络   |
| 常见 Node.js 依赖             | 单个 Rust 二进制文件                        |
| 本地桌面为中心                 | 无头 Linux、Docker、CI 优先          |
| 仅结果                           | 会话追踪、回放、诊断报告 |

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

显式指定：

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -t headless-use .
# 镜像内置 Chromium。单次运行：
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **沙箱注意：** Chromium 在 root 下运行时需要 `--no-sandbox`。在 Docker CI 中，这是隔离构建的可接受做法；对于可信的生产环境，建议以非 root 用户运行（附带的 Dockerfile 已这样做）并保持沙箱启用。参见 [docs/security.md](docs/security.md)。

## 支持的功能

- **真实输入**：`Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **鼠标**：移动、点击（左/右/中/后退/前进）、双击/三击、down/up、hold、hover、滚轮滚动、拖拽（插值）、drag-path
- **键盘**：down/up/press、组合键（`Control+Shift+P`）、type、insert-text（CJK/表情符号安全）、hold、repeat
- **观察**：无障碍/DOM 提取、语义 `@eN` 引用、边界框、stale 引用检测
- **诊断**：控制台 + 未捕获错误、网络（fetch/XHR）密钥屏蔽、wait-until-stable
- **截图**：视口、整页、元素
- **会话**：长期 `serve`（JSON-RPC stdio）、单次 `run`、追踪 + 回放
- **追踪**：`actions.jsonl`、截图、`report.html`（自包含）、密钥自动屏蔽
- **MCP 服务器**：基于 stdio 的规范兼容 `initialize`/`tools/list`/`tools/call`

## MCP 服务器

`headless-use mcp` 通过 stdio 运行规范兼容的 MCP 服务器（protocolVersion `2024-11-05`）。AI 代理无需包装 JSON-RPC 即可直接连接：

```bash
headless-use mcp --no-sandbox
```

服务器提供 18 个带有类型化 `inputSchema` 的 `browser_*` 工具。截图结果以 MCP 图像块返回，其他内容以简洁的 JSON 文本块返回。错误返回 `isError: true` 及恢复提示。

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

## CLI 参考

```
headless-use
├── launch      启动浏览器并保持运行
├── serve        启动基于 stdio 的长期 JSON-RPC 会话
├── run          执行单次操作并退出
├── doctor       诊断环境
├── install-browser   显示浏览器安装指南
└── mcp          启动基于 stdio 的 MCP 服务器
```

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

## 限制

- 跨域 iframe 操作有限；返回已记录的错误。
- OS 级 IME 输入未完全模拟；CJK/表情符号使用 `Input.insertText`。
- Firefox/WebKit 不在初始范围内（仅 Chromium）。
- 自动浏览器下载（`install-browser`）显示指南而非下载。
- 触摸输入（`touch tap`、`touch swipe`）在结构上受支持，但不在 MVP CLI 中。

## 社区

- [贡献指南](CONTRIBUTING.md) — 开发设置、代码标准、PR 流程
- [行为准则](CODE_OF_CONDUCT.md) — 社区标准
- [安全策略](SECURITY.md) — 漏洞报告
- [更新日志](CHANGELOG.md) — 发布历史
- [讨论](https://github.com/headless-use/headless-use/discussions) — 问题 & 想法

## 许可证

根据 Apache License, Version 2.0 授权。参见 [LICENSE](LICENSE)。
