# Security Policy

## Supported versions

No tagged release has been published yet. Only the `main` branch is supported;
build from source and update to the latest `main` to receive security fixes.

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

We will acknowledge receipt within 72 hours and aim to provide an initial
assessment within 7 days.

## Security boundaries

headless-use drives a browser on behalf of an AI agent. Key security properties:

- **CDP binds to `127.0.0.1` only** — the control plane is never exposed to
  external networks.
- **Secret masking** — traces and reports redact known-sensitive patterns
  (authorization headers, API keys, passwords) by default.
- **Path validation** — agent-supplied file paths are checked for `..` traversal.
- **Sandbox** — Chromium runs with the sandbox enabled by default; `--no-sandbox`
  is only auto-enabled when running as root (e.g., Docker) and prints a warning.

See [docs/security.md](docs/security.md) for the full security model.

## Disclosure policy

- We follow coordinated disclosure.
- A fix is prepared privately and a patched release is issued.
- Public disclosure follows after the release is available.
