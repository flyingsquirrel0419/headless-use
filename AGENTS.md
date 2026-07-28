# AGENTS.md — headless-use engineering guidelines

## Project knowledge system

### Principle
The structure of the project and important technical decisions are recorded inside
the repository. Documentation is part of the done criteria, not an optional add-on.

### Must-record items
- Overall project structure and each module's responsibility.
- Relationships between modules and main data flows.
- The tech stack used and the rationale/tradeoffs for each choice.
- The rationale for important decisions, recorded as `[Decision Log]` comments
  next to the code they describe (see below). This project keeps decision
  records in-source rather than as separate ADR files.
- Key information for building, testing, and deploying.

When a code change makes existing docs inaccurate, the docs are updated in the same
change. If docs and implementation disagree, the work is not considered done.

## Why-centered docs and comments

Code shows **how** something works. Docs and comments show **why** it was built this way.

The most important job of a comment is not to restate the code, but to explain:
- Why this code exists and its intent.
- The problem being solved and the constraints.
- Why this approach was chosen over alternatives.
- Pros/cons of the choice.
- Invariants that must hold when modifying.

Use the `[Decision Log]` format for new features, complex logic, or structural
changes. Write the entries in English so they are searchable and consistent
with the rest of the codebase:

```text
[Decision Log]
- Intent:
- Previous implementation and constraints:
- Alternatives considered:
- Chosen approach:
- Why this approach over the alternatives:
- Pros, cons, and impact:
```

## Module map

```
src/
├── browser/   process launch + CDP transport + Page + stealth (headless signal suppression)
├── cdp/       WebSocket JSON-RPC client, typed CDP types, errors
├── input/     mouse + keyboard engines (real Input.* events)
├── observe/   DOM-based interactive-element extraction + semantic @eN references,
│              screenshot annotation, dewiggle (pixel-only wobble reversal)
├── session/   high-level ops; resolve (@eN -> coords), capture (screenshot/
│             annotate/dewiggle), console, network, network_tracker, wait
├── viewer/    live MJPEG viewer: CDP screencast capture + localhost HTTP server
│              + cursor overlay
├── trace/     action recording + replay + report.html
├── protocol/  JSON-RPC over stdio
├── cli/       clap CLI + rpc dispatch + commands
├── mcp/       MCP tool definitions
├── security/  host allow/deny policy
└── util/      secrets masking, timestamps, fonts
```

## Key invariants (do not break)
- Input MUST use real `Input.dispatchMouseEvent`/`dispatchKeyEvent`/`insertText`.
  `element.click()` JS is only available via `invoke-click`, never the default.
- Coordinates are viewport CSS pixels, origin top-left.
- CDP raw JSON must not leak past the `cdp` module boundary.
- Secrets are masked in traces by default; `type` with `sensitive=true` redacts.
- The browser binds CDP to 127.0.0.1 only; never expose it to other interfaces.

## Building & testing
```bash
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace -- --test-threads=2
```
