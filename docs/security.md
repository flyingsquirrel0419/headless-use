# Security

AI agents drive the browser, so bounding blast radius matters.

## CDP exposure

- CDP binds to `127.0.0.1` only; it is never exposed to external interfaces.
- The HTTP debugging endpoints (`/json`) are likewise localhost-only.

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

## File uploads

File paths are validated against traversal (`..`) before use.

## Network policy

Host allow/deny lists are enforced on navigation (`open`/`goto`/`reload`).
Navigation to a disallowed host returns `NAVIGATION_BLOCKED` **before** any
request reaches the network. The deny list takes precedence over the allow list.

```
headless-use serve --allow-host localhost --allow-host 127.0.0.1
headless-use serve --deny-host evil.example.com
```

## Trace safety

Traces capture everything the agent does. Treat trace output as potentially
containing sensitive page content. Masking is conservative (may redact
non-secrets) but never leaks a known secret pattern.
