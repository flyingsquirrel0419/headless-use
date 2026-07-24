# Contributing to headless-use

Thank you for your interest in contributing! This guide covers the development
workflow, code standards, and how to submit changes.

## Prerequisites

- Rust 1.75+ (stable or nightly)
- A Chromium-based browser on `PATH` (or set `HEADLESS_USE_BROWSER_PATH`)
- Linux recommended; macOS works for development

## Development setup

```bash
git clone <repo-url> && cd headless-use
cargo build
cargo test --workspace -- --test-threads=2
```

Integration tests launch a real headless browser. On Linux as root, the runtime
auto-enables `--no-sandbox`; set `HEADLESS_USE_BROWSER_PATH` if your browser is
not on `PATH`.

## Code standards

All contributions must pass:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace -- --test-threads=2
```

### Key invariants (do not break)

- **Real input only**: mouse/keyboard use `Input.dispatchMouseEvent` /
  `dispatchKeyEvent` / `insertText`. Never `element.click()` as the default.
- **Coordinates**: viewport CSS pixels, origin top-left.
- **CDP encapsulation**: raw CDP JSON must not leak past the `cdp` module.
- **Secret masking**: traces redact by default; `type` with `sensitive=true`
  stores `[REDACTED]`.
- **Localhost binding**: CDP binds to `127.0.0.1` only.

### Why-centered comments

Code shows *how*; comments show *why*. Use the `[Decision Log]` format for new
features or structural changes (see `AGENTS.md`).

## Adding a new tool (MCP / JSON-RPC)

1. Add the tool definition to `src/mcp/schema.rs` (name, description, inputSchema).
2. Add the method mapping in `tool_for_method`.
3. Implement the dispatch case in `src/cli/rpc.rs`.
4. Add a fixture if needed in `tests/fixtures/`.
5. Add an integration test in `tests/`.
6. Update `docs/protocol.md` and the README tool list.

## Pull request process

1. Fork and create a branch from `main`.
2. Write tests for new behavior.
3. Ensure `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` all pass.
4. Use the PR template (`.github/pull_request_template.md`).
5. Keep changes focused; one feature or fix per PR.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add touch swipe support
fix: resolve Korean element click injection
docs: add Japanese README
```

## Reporting issues

Use the GitHub issue templates. For security vulnerabilities, see
[SECURITY.md](SECURITY.md) — do **not** open a public issue.
