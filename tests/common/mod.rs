//! Shared test helpers: fixture HTTP server + browser launch.
//!
//! Every integration binary compiles this module in full but uses a different
//! slice of it, so unused-item warnings here say nothing about whether an item
//! is actually dead. Allowed once at the top rather than per item.
#![allow(dead_code)]

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
    ///
    /// Bound to `0.0.0.0` rather than `127.0.0.1` so the same server is
    /// reachable under more than one host string — `127.0.0.1` and `127.0.0.2`
    /// are both loopback. Host-policy tests need a denied host that genuinely
    /// resolves and connects; pointing them at an unroutable name like
    /// `evil.example.com` would pass whether or not the policy did anything,
    /// because DNS would fail first.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
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

    /// The same fixture, addressed through a different host string.
    pub fn url_via(&self, host: &str, name: &str) -> String {
        format!("http://{host}:{}/{name}", self._port)
    }

    /// A URL on this server that 302-redirects to `target`.
    pub fn redirect_to(&self, target: &str) -> String {
        format!("{}/redirect?to={target}", self.base_url)
    }

    /// The port this server listens on.
    pub fn port(&self) -> u16 {
        self._port
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
            } else if let Some(target) = path.strip_prefix("redirect?to=") {
                // Server-side 302. Used to prove the host policy is enforced on
                // the *redirected* request and not only on the URL the caller
                // originally asked for.
                let target = target.to_string();
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
                return;
            } else if path == "slow" {
                // Genuinely slow endpoint: sleeps 1.5s before responding.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                (
                    "HTTP/1.1 200 OK".to_string(),
                    b"slow response".to_vec(),
                    "text/plain",
                )
            } else {
                // Strip any query string, then refuse traversal outright: a
                // fixture server that serves `../../` is a foot-gun even in a
                // test binary, and it silently made "file not found" assertions
                // depend on the checkout layout.
                let rel = path.split('?').next().unwrap_or("");
                let safe = !rel.is_empty()
                    && std::path::Path::new(rel)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                let file_path = if rel.is_empty() {
                    root.join("basic-form.html")
                } else if safe {
                    root.join(rel)
                } else {
                    root.join("\0nonexistent")
                };
                // A missing fixture must answer 404. Returning 200 with a
                // "Not Found" body made it impossible to test error handling,
                // and hid fixture typos behind a successful-looking response.
                match std::fs::read(&file_path) {
                    Ok(b) => ("HTTP/1.1 200 OK".to_string(), b, mime_for(&file_path)),
                    Err(_) => (
                        "HTTP/1.1 404 Not Found".to_string(),
                        b"Not Found".to_vec(),
                        "text/plain",
                    ),
                }
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

/// Let a spawned `serve`/`mcp` child exit on its own, killing it only if it
/// refuses.
///
/// `Child::kill` sends SIGKILL, which cannot be caught — so a test that reaches
/// for it immediately denies the child any chance to run its cleanup, leaving a
/// Chrome process and a temp profile directory behind on every run. Dropping
/// the child's stdin already makes the stdio loop see EOF and return; this just
/// waits for that to happen.
pub fn shutdown_child(mut child: std::process::Child) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Launch options suitable for tests (root + no-sandbox).
///
/// Prefer [`TempProfile::launch_opts`]: options built here get a library-owned
/// profile directory, which the suite cannot clean up reliably (see
/// [`TempProfile`]).
pub fn test_launch() -> headless_use::LaunchOptions {
    headless_use::LaunchOptions {
        no_sandbox: true,
        ..Default::default()
    }
}

/// A browser profile directory owned by the test, removed when it drops.
///
/// ## Why tests own the profile
/// With no `user_data_dir` set the library generates `/tmp/headless-use-<hex>`
/// and removes it during `Session::shutdown`. That removal *succeeds* — and
/// then Chrome's helper processes, which outlive the SIGKILL'd main process,
/// recreate `<profile>/Default/...` underneath it. Nothing looks at the path
/// again, so the husk stays in `/tmp`. It only shows up under load
/// (`--test-threads=2` reproduces it; `--test-threads=1` mostly does not),
/// which is why a full `cargo test --workspace` run used to leave a handful of
/// `/tmp/headless-use-*` directories behind.
///
/// Pinning the path here lets the test re-remove until the removal sticks. It
/// also moves the litter out of the `headless-use-*` namespace, so anything
/// still matching that glob after a suite run is a library-side leak rather
/// than test hygiene.
pub struct TempProfile {
    path: PathBuf,
}

impl TempProfile {
    /// Reserve a unique profile path. The directory itself is created by
    /// Chrome.
    pub fn new() -> Self {
        let bytes: [u8; 8] = std::array::from_fn(|_| fastrand::u8(0..=255));
        let path = std::env::temp_dir().join(format!("hu-test-profile-{}", hex::encode(bytes)));
        Self { path }
    }

    /// [`test_launch`] options pointed at this profile.
    pub fn launch_opts(&self) -> headless_use::LaunchOptions {
        headless_use::LaunchOptions {
            user_data_dir: Some(self.path.clone()),
            ..test_launch()
        }
    }

    /// The reserved path, for tests that build their own `LaunchOptions`.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Default for TempProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempProfile {
    /// Removing a directory is pure `std::fs`, so — unlike `Session::shutdown`,
    /// which consumes `self` and is `async` and therefore cannot be driven from
    /// `Drop` at all without a `block_on` that would panic on a runtime thread —
    /// this needs no runtime and belongs here. Tests still shut their session
    /// down explicitly; this guard only owns the directory.
    fn drop(&mut self) {
        remove_profile_dir(&self.path);
    }
}

/// Remove `dir`, waiting out the Chrome processes that are still using it.
///
/// Removing once is not enough. `remove_dir_all` on a profile a dying Chrome
/// still holds *succeeds* and is then undone: helper processes (network
/// service, storage service, zygotes) outlive the killed main process and
/// recreate `Default/Cache/...` underneath. Measured across this suite that
/// happens at most once per profile and within ~50ms of the first removal, so
/// this deletes repeatedly until the directory has stayed gone for
/// `QUIET_FOR`, with `BUDGET` as the backstop for a browser that never died.
///
/// Pure `std::fs` + `std::thread::sleep`, so it is safe to call from `Drop`.
pub fn remove_profile_dir(dir: &std::path::Path) {
    /// How long the directory must stay absent before we believe it.
    const QUIET_FOR: std::time::Duration = std::time::Duration::from_millis(300);
    /// Cap, so a browser that is still running cannot stall the suite.
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    let start = std::time::Instant::now();
    let mut last_present = start;
    loop {
        let _ = std::fs::remove_dir_all(dir);
        if dir.exists() {
            last_present = std::time::Instant::now();
        } else if last_present.elapsed() >= QUIET_FOR {
            return;
        }
        if start.elapsed() >= BUDGET {
            eprintln!(
                "warning: test profile {} could not be removed; a browser is probably still \
                 running against it",
                dir.display()
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
