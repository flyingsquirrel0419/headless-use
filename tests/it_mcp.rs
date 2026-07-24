//! MCP server integration tests.
//!
//! Drives the `headless-use mcp` subprocess as a real MCP client would:
//! initialize → notifications/initialized → tools/list → tools/call, verifying
//! protocol compliance and end-to-end browser operation.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn binary() -> String {
    env!("CARGO_BIN_EXE_headless-use").to_string()
}

/// Send one JSON-RPC line to the subprocess stdin.
fn send(stdin: &mut impl Write, method: &str, params: Value, id: Option<i64>) {
    let mut req = json!({ "method": method, "params": params, "jsonrpc": "2.0" });
    if let Some(id) = id {
        req["id"] = json!(id);
    }
    let line = serde_json::to_string(&req).unwrap();
    writeln!(stdin, "{line}").unwrap();
    stdin.flush().unwrap();
}

/// Read one JSON response line (skip notifications which have no id).
fn recv(reader: &mut impl BufRead, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    let mut line = String::new();
    loop {
        if Instant::now() > deadline {
            return None;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Only return responses (have an id). Skip notifications.
                if v.get("id").is_some() {
                    return Some(v);
                }
            }
            Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_handshake_and_tools_list() {
    common::init();
    let mut child = Command::new(binary())
        .args(["mcp", "--no-sandbox"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    // 1. initialize
    send(
        &mut stdin,
        "initialize",
        json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }),
        Some(1),
    );
    let r = recv(&mut stdout, to).expect("initialize response");
    assert_eq!(r["result"]["protocolVersion"], json!("2024-11-05"));
    assert_eq!(r["result"]["serverInfo"]["name"], json!("headless-use"));
    assert!(r["result"]["capabilities"]["tools"].is_object());

    // 2. notifications/initialized (no response)
    send(&mut stdin, "notifications/initialized", json!({}), None);
    std::thread::sleep(Duration::from_millis(100));

    // 3. tools/list
    send(&mut stdin, "tools/list", json!({}), Some(2));
    let r2 = recv(&mut stdout, to).expect("tools/list response");
    let tools = r2["result"]["tools"].as_array().unwrap();
    assert!(
        tools.len() >= 18,
        "expected >=18 tools, got {}",
        tools.len()
    );
    // Each tool must have name, description, inputSchema.
    for t in tools {
        assert!(t["name"].is_string(), "tool missing name: {t}");
        assert!(t["description"].is_string());
        assert_eq!(t["inputSchema"]["type"], "object");
    }
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"browser_observe"));
    assert!(names.contains(&"browser_click"));

    send(&mut stdin, "browser.close", json!({}), Some(99));
    let _ = recv(&mut stdout, to);
    drop(stdin);
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_full_browser_flow() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let mut child = Command::new(binary())
        .args(["mcp", "--no-sandbox"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    // Handshake
    send(
        &mut stdin,
        "initialize",
        json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }),
        Some(1),
    );
    let _ = recv(&mut stdout, to).expect("initialize");
    send(&mut stdin, "notifications/initialized", json!({}), None);
    std::thread::sleep(Duration::from_millis(100));

    // browser_open
    send(
        &mut stdin,
        "tools/call",
        json!({ "name": "browser_open", "arguments": { "url": srv.url("basic-form.html") } }),
        Some(10),
    );
    let r = recv(&mut stdout, to).expect("open response");
    assert_eq!(r["result"]["isError"], json!(false));
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("basic-form.html"));

    // browser_observe
    send(
        &mut stdin,
        "tools/call",
        json!({ "name": "browser_observe", "arguments": {} }),
        Some(11),
    );
    let r = recv(&mut stdout, to).expect("observe response");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    let obs: Value = serde_json::from_str(text).unwrap();
    let elements = obs["elements"].as_array().unwrap();
    assert!(elements.len() >= 4);

    // Find email textbox ref
    let email_ref = elements
        .iter()
        .find(|e| e["role"] == "textbox" && e["name"].as_str().unwrap().contains("이메일"))
        .map(|e| format!("@e{}", e["ref_id"].as_u64().unwrap()))
        .unwrap();

    // browser_click the email field
    send(
        &mut stdin,
        "tools/call",
        json!({ "name": "browser_click", "arguments": { "ref": email_ref } }),
        Some(12),
    );
    let r = recv(&mut stdout, to).expect("click response");
    assert_eq!(r["result"]["isError"], json!(false));

    // browser_type
    send(
        &mut stdin,
        "tools/call",
        json!({ "name": "browser_type", "arguments": { "text": "mcp@example.com" } }),
        Some(13),
    );
    let r = recv(&mut stdout, to).expect("type response");
    assert_eq!(r["result"]["isError"], json!(false));

    // browser_screenshot returns an image block
    send(
        &mut stdin,
        "tools/call",
        json!({ "name": "browser_screenshot", "arguments": { "fullPage": false } }),
        Some(14),
    );
    let r = recv(&mut stdout, to).expect("screenshot response");
    assert_eq!(r["result"]["content"][0]["type"], "image");
    assert_eq!(r["result"]["content"][0]["mimeType"], "image/png");
    let data = r["result"]["content"][0]["data"].as_str().unwrap();
    assert!(data.len() > 100, "screenshot base64 too short");

    send(&mut stdin, "browser.close", json!({}), Some(99));
    let _ = recv(&mut stdout, to);
    drop(stdin);
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_error_before_initialize_rejected() {
    common::init();
    let mut child = Command::new(binary())
        .args(["mcp", "--no-sandbox"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(10);

    // Call tools/list BEFORE initialize → should get an error.
    send(&mut stdin, "tools/list", json!({}), Some(1));
    let r = recv(&mut stdout, to).expect("error response");
    assert!(r.get("error").is_some(), "expected error before init: {r}");
    assert_eq!(r["error"]["code"], json!(-32002));

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}
