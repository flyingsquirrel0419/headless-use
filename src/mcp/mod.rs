//! Model Context Protocol (MCP) server over stdio.
//!
//! Implements the MCP specification (protocolVersion "2024-11-05") so AI coding
//! agents can drive the browser without wrapping the raw JSON-RPC protocol.
//!
//! ## Why a dedicated MCP transport (not just `serve`)
//! MCP has a required handshake: `initialize` → `notifications/initialized`
//! before any tool call, and tool results must be wrapped in `content` blocks.
//! MCP notifications carry no `id`, which the plain JSON-RPC `Request` parser
//! rejects. A separate transport handles notifications and keeps both surfaces
//! behaviorally identical via the shared [`crate::cli::rpc::dispatch`].
//!
//! ## Lifecycle
//! 1. Client sends `initialize` → server replies with capabilities + serverInfo.
//! 2. Client sends `notifications/initialized` (no response).
//! 3. Client calls `tools/list` → tool definitions.
//! 4. Client calls `tools/call` with `{name, arguments}` → `content` blocks.

pub mod schema;

/// MCP protocol version this server implements.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name reported in the `initialize` response.
pub const SERVER_NAME: &str = "headless-use";

/// The stdio MCP transport loop.
pub mod transport {
    use std::io::{self, BufRead, Write};

    use serde_json::{json, Value};

    use crate::cli::rpc;
    use crate::mcp::{schema, PROTOCOL_VERSION, SERVER_NAME};
    use crate::session::Session;

    /// Run the MCP stdio loop until stdin closes.
    pub async fn run(session: Session) -> i32 {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut stdout = io::stdout();
        let mut stderr = io::stderr();

        let mut initialized = false;

        while let Some(line) = read_line(&mut reader) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    let _ = writeln!(stderr, "mcp parse error: {e}");
                    continue;
                }
            };

            let method = match msg.get("method").and_then(|m| m.as_str()) {
                Some(m) => m.to_string(),
                None => continue,
            };
            let id = msg.get("id").cloned();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);

            // Notifications have no id and get no response.
            if id.is_none() {
                if method == "notifications/initialized" {
                    initialized = true;
                }
                continue;
            }
            // id is Some here (it was a request, not a notification).
            let id = id.unwrap();

            match method.as_str() {
                "initialize" => {
                    let result = json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": SERVER_NAME, "version": crate::VERSION }
                    });
                    write_response(&mut stdout, &id, &result);
                }
                "tools/list" => {
                    if !initialized {
                        write_error(&mut stdout, &id, -32002, "Server not initialized");
                        continue;
                    }
                    let result = json!({ "tools": schema::tool_definitions() });
                    write_response(&mut stdout, &id, &result);
                }
                "tools/call" => {
                    if !initialized {
                        write_error(&mut stdout, &id, -32002, "Server not initialized");
                        continue;
                    }
                    handle_tool_call(&session, &params, &id, &mut stdout).await;
                }
                _ => {
                    write_error(
                        &mut stdout,
                        &id,
                        -32601,
                        &format!("unknown method: {method}"),
                    );
                }
            }
            let _ = stdout.flush();
        }
        0
    }

    /// Handle a single `tools/call` request.
    async fn handle_tool_call(
        session: &Session,
        params: &Value,
        id: &Value,
        stdout: &mut impl Write,
    ) {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let Some(method) = schema::tool_for_method(name) else {
            let result = json!({
                "content": [{ "type": "text", "text": format!("Unknown tool: {name}") }],
                "isError": true,
            });
            write_response(stdout, id, &result);
            return;
        };

        match rpc::dispatch(session, method, &arguments).await {
            Ok(value) => {
                let content = build_success_content(name, &value);
                let result = json!({ "content": content, "isError": false });
                write_response(stdout, id, &result);
            }
            Err(e) => {
                let result = json!({
                    "content": [{
                        "type": "text",
                        "text": format!("{}: {}\nRecovery: {}", e.code(), e, e.recovery())
                    }],
                    "isError": true,
                });
                write_response(stdout, id, &result);
            }
        }
    }

    /// Build content blocks for a successful tool call.
    ///
    /// Screenshots return an image block (base64 PNG); everything else returns a
    /// text block with compact JSON so agents get structured data cheaply.
    fn build_success_content(tool: &str, value: &Value) -> Vec<Value> {
        if tool == "browser_screenshot" {
            if let Some(data) = value.get("data").and_then(|v| v.as_str()) {
                return vec![json!({ "type": "image", "data": data, "mimeType": "image/png" })];
            }
        }
        let text = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
        vec![json!({ "type": "text", "text": text })]
    }

    /// Read one line from a BufRead, returning None on EOF.
    fn read_line(reader: &mut impl BufRead) -> Option<String> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                // Retry once; if still WouldBlock, give up to avoid a spin.
                line.clear();
                reader.read_line(&mut line).ok().map(|_| line)
            }
            Err(_) => None,
        }
    }

    fn write_response(stdout: &mut impl Write, id: &Value, result: &Value) {
        let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
        let _ = stdout.flush();
    }

    fn write_error(stdout: &mut impl Write, id: &Value, code: i64, message: &str) {
        let resp =
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
        let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
        let _ = stdout.flush();
    }
}
