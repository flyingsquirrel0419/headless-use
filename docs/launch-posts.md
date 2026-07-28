# Launch posts

## X

I just released headless-use 1.0.

It gives coding agents a real browser on headless Linux: real mouse and
keyboard input, semantic refs, screenshots, console/network diagnostics, MCP,
and JSON-RPC.

No Playwright. No Xvfb.

https://github.com/flyingsquirrel0419/headless-use

`npm install -g headless-use`

## Show HN

**Title:** Show HN: headless-use – A browser runtime for coding agents on headless Linux

I built headless-use because I do a lot of coding on remote Linux servers and
kept running into the same gap: the coding agent could change a web app, but it
couldn't actually look at the result and operate it in that environment.

This isn't a Playwright wrapper. It's a Rust binary that launches Chromium and
speaks CDP directly. It combines real mouse and keyboard events with compact
semantic element references, plus screenshots and console/network diagnostics,
so an existing coding agent can inspect and verify the app it is building.

You can use it over MCP or JSON-RPC and install it from npm, Cargo, or Docker:

`npm install -g headless-use`

https://github.com/flyingsquirrel0419/headless-use

I'd especially like feedback on the semantic-reference model, remote/CI setup,
and which diagnostics are most useful during real agent coding sessions.

## Comparison response

headless-use is not its own LLM agent. It is a lightweight browser runtime for
the coding agent you already use, aimed at remote Linux and CI. Unlike a
Playwright wrapper, it controls Chromium directly over CDP and combines real
input events with semantic references, screenshots, and structured
console/network diagnostics. If Playwright MCP already fits your workflow, keep
using it; headless-use is for cases where a small agent-first runtime and no
Node.js/Xvfb dependency are useful.
