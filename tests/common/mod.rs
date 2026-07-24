//! Shared test helpers: fixture HTTP server + browser launch.

use std::path::PathBuf;
use std::sync::Once;
use tokio::net::TcpListener;

static INIT: Once = Once::new();

/// Ensure tracing is initialized once.
pub fn init() {
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_writer(std::io::stderr)
            .try_init();
    });
}

/// A tiny static file HTTP server for fixtures.
pub struct FixtureServer {
    pub base_url: String,
    _handle: tokio::task::JoinHandle<()>,
    _port: u16,
}

impl FixtureServer {
    /// Start a server rooted at the `tests/fixtures` directory.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");
        let fixtures_dir = fixtures_dir();
        let handle = tokio::spawn(async move {
            serve(listener, fixtures_dir).await;
        });
        Self {
            base_url,
            _handle: handle,
            _port: port,
        }
    }

    /// URL for a named fixture.
    pub fn url(&self, name: &str) -> String {
        format!("{}/{}", self.base_url, name)
    }
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

async fn serve(listener: TcpListener, root: PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let root = root.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match sock.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            // Special API routes for the network-errors fixture.
            let (status_line, body, mime) = if path == "api/bad" {
                (
                    "HTTP/1.1 500 Internal Server Error".to_string(),
                    b"server error".to_vec(),
                    "text/plain",
                )
            } else if path == "api/ok" {
                (
                    "HTTP/1.1 200 OK".to_string(),
                    b"{\"ok\":true}".to_vec(),
                    "application/json",
                )
            } else if path == "slow" {
                // Genuinely slow endpoint: sleeps 1.5s before responding.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                (
                    "HTTP/1.1 200 OK".to_string(),
                    b"slow response".to_vec(),
                    "text/plain",
                )
            } else {
                let file_path = if path.is_empty() {
                    root.join("basic-form.html")
                } else {
                    root.join(&path)
                };
                let body = match std::fs::read(&file_path) {
                    Ok(b) => b,
                    Err(_) => b"Not Found".to_vec(),
                };
                ("HTTP/1.1 200 OK".to_string(), body, mime_for(&file_path))
            };
            let resp = format!(
                "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                status_line,
                mime,
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
        });
    }
}

fn mime_for(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Launch options suitable for tests (root + no-sandbox).
#[allow(dead_code)]
pub fn test_launch() -> headless_use::LaunchOptions {
    headless_use::LaunchOptions {
        no_sandbox: true,
        ..Default::default()
    }
}
