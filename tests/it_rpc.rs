//! Integration test for the stdio JSON-RPC `serve` mode.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn binary() -> String {
    env!("CARGO_BIN_EXE_headless-use").to_string()
}

fn send(stdin: &mut impl Write, method: &str, params: Value, id: i64) {
    let req = json!({ "id": id, "method": method, "params": params, "jsonrpc": "2.0" });
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    stdin.flush().unwrap();
}

/// Read one JSON line with an overall deadline. Skips blank lines.
fn recv(reader: &mut impl BufRead, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    let mut line = String::new();
    loop {
        if Instant::now() > deadline {
            return None;
        }
        line.clear();
        // Non-blocking-ish: read_line blocks until data or EOF. Since serve writes
        // one line per response, this returns promptly when a response arrives.
        match reader.read_line(&mut line) {
            Ok(0) => return None, // EOF
            Ok(_) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                return serde_json::from_str(t).ok();
            }
            Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_rpc_observe_click_type() {
    common::init();
    let srv = common::FixtureServer::start().await;
    let mut child = Command::new(binary())
        .args(["serve", "--no-sandbox"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let to = Duration::from_secs(25);

    send(
        &mut stdin,
        "browser.open",
        json!({ "url": srv.url("basic-form.html") }),
        1,
    );
    let r1 = recv(&mut stdout, to).expect("open response");
    assert!(r1["result"]["url"]
        .as_str()
        .unwrap()
        .contains("basic-form.html"));

    send(&mut stdin, "observe", json!({}), 2);
    let r2 = recv(&mut stdout, to).expect("observe response");
    let elements = r2["result"]["elements"].as_array().unwrap();
    assert!(elements.len() >= 4);

    let email_ref = elements
        .iter()
        .find(|e| e["role"] == "textbox" && e["name"].as_str().unwrap().contains("이메일"))
        .map(|e| e["ref_id"].as_u64().unwrap())
        .unwrap();
    send(
        &mut stdin,
        "click",
        json!({ "ref": format!("@e{}", email_ref) }),
        3,
    );
    let r3 = recv(&mut stdout, to).expect("click response");
    assert_eq!(r3["result"]["success"], json!(true));

    send(
        &mut stdin,
        "type",
        json!({ "text": "agent@example.com" }),
        4,
    );
    let r4 = recv(&mut stdout, to).expect("type response");
    assert_eq!(r4["result"]["success"], json!(true));

    send(&mut stdin, "observe", json!({}), 5);
    let r5 = recv(&mut stdout, to).expect("observe2 response");
    let val = r5["result"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == "textbox" && e["name"].as_str().unwrap().contains("이메일"))
        .and_then(|e| e["value"].as_str())
        .map(String::from);
    assert_eq!(val.as_deref(), Some("agent@example.com"));

    send(&mut stdin, "browser.close", json!({}), 99);
    let _ = recv(&mut stdout, to);
    drop(stdin);
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
}
