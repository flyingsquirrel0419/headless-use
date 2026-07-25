# Security

AI agents drive the browser, so bounding blast radius matters.

## CDP exposure

- CDP binds to `127.0.0.1` only; it is never exposed to external interfaces.
- The HTTP debugging endpoints (`/json`) are likewise localhost-only.

## Live viewer exposure

`headless-use view` serves an MJPEG stream of the agent-controlled page. It
binds to `127.0.0.1` by default.

`--viewer-host 0.0.0.0` widens that bind for remote viewing. **The stream is
unauthenticated**: anyone who can reach the address can watch whatever the page
is showing, including logged-in session content. `headless-use` prints a warning
whenever the viewer binds to a non-loopback address. Use it only on a trusted
network, or put it behind an authenticating reverse proxy.

The viewer is a separate surface from CDP — widening the viewer does not expose
the CDP endpoint, which stays loopback-only.

## Site isolation

Chrome's site isolation (`site-per-process`, `IsolateOrigins`) is left **on**.
It costs memory but it is the main boundary between a visited page and the rest
of the browser, which matters when an agent chooses the URLs.

## Sandbox

Chromium refuses to run as root without `--no-sandbox`. In Docker/CI as root
this is acceptable for isolated, trusted builds. The shipped Dockerfile runs as
a non-root user to keep the sandbox on by default. `headless-use` prints a
warning when `--no-sandbox` is used.

## Sensitive data masking

By default, traces and reports redact:
- `Authorization`, `Cookie`, `Set-Cookie`, `X-API-Key` headers
- `.env`-style `KEY=VALUE` for secret-ish keys (`*key*`, `*secret*`, `*token*`, `*password*`)
- bearer tokens
- `type` with `sensitive: true` stores `[REDACTED]` instead of the value

Network URLs are masked; bodies are not stored by default.

## File paths

Paths that arrive over JSON-RPC/MCP are chosen by the *agent*, so they are
resolved against the process working directory and rejected if they contain
`..` or otherwise escape it. This covers `trace.start` (`base`) and `replay`
(`runDir`).

Paths passed as CLI arguments (`run --screenshot out.png`) are not restricted:
they come from the operator, who already has a shell.

## Network policy

Host allow/deny lists are enforced on navigation (`open`/`goto`/`reload`).
Navigation to a disallowed host returns `NAVIGATION_BLOCKED` **before** any
request reaches the network. The deny list takes precedence over the allow list.

```
headless-use serve --allow-host localhost --allow-host 127.0.0.1
headless-use serve --deny-host evil.example.com
```

Once any allow or deny host is configured, navigation is also restricted to
`http`/`https` (plus `about:blank`). Without this, `file:///etc/passwd`,
`data:` and `javascript:` URLs would slip past every host rule — they have no
host for a rule to match. With no policy configured the default is permissive.

## Trace safety

Traces capture everything the agent does. Treat trace output as potentially
containing sensitive page content. Masking is conservative (may redact
non-secrets) but never leaks a known secret pattern.
