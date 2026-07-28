# Security Policy

## Supported versions

Only the `main` branch is supported; build from source and update to the latest
`main` to receive security fixes.

## Reporting a vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please report privately via GitHub Security Advisories:
https://github.com/flyingsquirrel0419/headless-use/security/advisories/new
(equivalently, the repository's **Security** tab → **Report a vulnerability**).

Include:
- A description of the vulnerability and its impact
- Steps to reproduce (proof of concept)
- Affected commit (`git rev-parse HEAD`) or `headless-use --version`
- Suggested fix if you have one

---

# Security architecture

AI agents drive the browser, so bounding blast radius matters.

## CDP exposure

- CDP binds to `127.0.0.1` only; it is never exposed to external interfaces.
- The HTTP debugging endpoints (`/json`) are likewise localhost-only.

## Live viewer exposure

`headless-use view` serves an MJPEG stream of the agent-controlled page. It
binds to `127.0.0.1` by default. `--viewer-host 0.0.0.0` widens that bind for
remote viewing.

### Access token

Every `view` run has an access token. It is generated at startup unless you pin
one with `--viewer-token <TOKEN>`, and it is passed as a query parameter:

```
http://HOST:7780/?token=<TOKEN>
http://HOST:7780/stream?token=<TOKEN>
```

It is a query parameter and not an `Authorization` header because the point is
that you can paste the printed URL into a browser; a browser cannot be told to
attach a header to a top-level navigation or to an `<img>` load. `view` prints
the full URL — token included — to **stderr** at startup (stdout carries
JSON-RPC), or in the `--json` banner as `viewer.url` / `viewer.token`.

### Which mode am I in?

Enforcement depends **only on the bind address**:

| `--viewer-host` | Token generated & printed | Request without a valid token |
| --- | --- | --- |
| loopback (`127.0.0.1` default, `::1`, `127.0.0.2`, …) | yes | **served** — token optional |
| anything else (`0.0.0.0`, a LAN/public address) | yes | **`401 Unauthorized`** |

The `--json` banner reports this directly as `viewer.token_required`, and the
plain-text banner prints an `Access:` line saying which mode is active.

Loopback is deliberately permissive: anything that can open a loopback socket
already runs as this user and can read the token out of the process' own argv
or output, so demanding it there would break `headless-use view` + click-the-
link without buying anything. The non-loopback bind is the case that is
genuinely reachable by other machines, so there the token is mandatory.

`/health` answers `ok` without a token in both modes. It returns a fixed string
and discloses nothing about the page, so uptime probes do not need the secret.

Comparison of the supplied token against the expected one is byte-wise without
an early return, so a wrong token does not leak how much of it was right.

### What the token does not protect against

- **It is a bearer token in a URL.** Anyone who obtains the URL is in. It will
  land in your shell history, in the browser's history and autocomplete, and in
  the `Referer` header of any request the page makes to a third party.
- **The stream is unencrypted HTTP.** Anyone who can observe the traffic
  between the browser and the viewer sees both the token and the frames. On an
  untrusted network, tunnel it (SSH port-forward, WireGuard) or put it behind
  a TLS-terminating reverse proxy.
- **It is not a CSPRNG value.** The generated token is 128 bits from
  `fastrand`, which is a PRNG. It defeats casual scanning of an exposed port,
  not a determined attacker who can observe other outputs of the same PRNG.
- **It is per-process, and there is no revocation, expiry, or rate limiting.**
  It is valid for as long as that `view` process runs.
- It does not restrict *what* the viewer shows. The stream still carries
  whatever the agent-controlled page is showing, including logged-in session
  content.

`headless-use` still prints a warning whenever the viewer binds to a
non-loopback address, for the reasons above.

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
