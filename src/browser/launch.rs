//! Browser discovery, launch arguments, and process lifecycle.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::{Child, Command};

use crate::browser::BrowserError;

/// Candidate browser executable names, in priority order.
const BROWSER_CANDIDATES: &[&str] = &[
    "chrome-headless-shell",
    "chromium-headless-shell",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
];

/// Compatibility mode for the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompatMode {
    /// Modern Chrome headless (`--headless=new`). Default.
    #[default]
    Chromium,
    /// Chromium launched under Xvfb (real display). Fallback only.
    Xvfb,
}

/// Options controlling browser launch.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Explicit browser executable path. If `None`, discover via [`discover_browser`].
    pub browser_path: Option<PathBuf>,
    /// Viewport in CSS pixels.
    pub viewport: crate::cdp::Viewport,
    /// User data directory. If `None`, a temp dir is created and removed on exit.
    pub user_data_dir: Option<PathBuf>,
    /// Incognito mode (fresh profile, no persistence).
    pub incognito: bool,
    /// HTTP proxy URL, e.g. `http://127.0.0.1:8080`.
    pub proxy: Option<String>,
    /// Compatibility mode.
    pub compat: CompatMode,
    /// Disable the Chromium sandbox (required as root in Docker).
    /// Off by default for safety; `doctor` and Docker document when it's needed.
    pub no_sandbox: bool,
    /// Additional Chrome flags appended verbatim.
    pub extra_args: Vec<String>,
    /// Port for the CDP HTTP/WS endpoint. 0 = auto-pick a free port.
    pub port: u16,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            browser_path: None,
            viewport: crate::cdp::Viewport {
                width: 1280,
                height: 720,
                device_scale_factor: 1.0,
            },
            user_data_dir: None,
            incognito: false,
            proxy: None,
            compat: CompatMode::Chromium,
            no_sandbox: false,
            extra_args: Vec::new(),
            port: 0,
        }
    }
}

impl LaunchOptions {
    /// Builder: set viewport from a `WxH` string.
    pub fn with_viewport(mut self, v: crate::cdp::Viewport) -> Self {
        self.viewport = v;
        self
    }
}

/// Discover a browser executable.
///
/// Order: `HEADLESS_USE_BROWSER_PATH` env, then the candidates on `PATH`.
pub fn discover_browser() -> Result<PathBuf, BrowserError> {
    if let Ok(p) = std::env::var("HEADLESS_USE_BROWSER_PATH") {
        let path = PathBuf::from(&p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(BrowserError::BrowserNotFound(format!(
            "HEADLESS_USE_BROWSER_PATH={p} does not exist"
        )));
    }
    for name in BROWSER_CANDIDATES {
        if let Some(p) = which(name) {
            return Ok(p);
        }
    }
    Err(BrowserError::BrowserNotFound(format!(
        "no browser found in PATH (tried: {}). Set HEADLESS_USE_BROWSER_PATH or install one.",
        BROWSER_CANDIDATES.join(", ")
    )))
}

/// Locate an executable on PATH (no external dep).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            // Best-effort executable check.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&candidate) {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return Some(candidate);
                    }
                }
            }
            #[cfg(not(unix))]
            return Some(candidate);
        }
    }
    None
}

/// A spawned browser process + its temp resources.
pub struct BrowserProcess {
    child: tokio::sync::Mutex<Option<Child>>,
    exe: PathBuf,
    port: u16,
    temp_dir: Option<PathBuf>,
    owns_temp: bool,
    xvfb_display: Option<String>,
}

impl BrowserProcess {
    /// Launch Chrome and wait until the CDP HTTP endpoint responds.
    pub async fn launch(opts: LaunchOptions) -> Result<Self, BrowserError> {
        let exe = match opts.browser_path.clone() {
            Some(p) if p.is_file() => p,
            _ => discover_browser()?,
        };

        let port = if opts.port == 0 {
            pick_free_port().ok_or_else(|| BrowserError::LaunchFailed("no free port".into()))?
        } else {
            opts.port
        };

        // Temp user-data dir unless one is provided.
        let (temp_dir, owns_temp) = match &opts.user_data_dir {
            Some(d) => (d.clone(), false),
            None => {
                let d = std::env::temp_dir().join(format!("headless-use-{}", short_uuid()));
                std::fs::create_dir_all(&d).map_err(BrowserError::Io)?;
                (d, true)
            }
        };

        // Xvfb handling.
        let xvfb_display = match opts.compat {
            CompatMode::Xvfb => Some(start_xvfb().await?),
            CompatMode::Chromium => None,
        };
        let mut display_env = Vec::new();
        if let Some(d) = &xvfb_display {
            display_env.push(("DISPLAY", d.clone()));
        }

        let mut args: Vec<String> = Vec::with_capacity(16);
        match opts.compat {
            CompatMode::Chromium => {
                args.push("--headless=new".into());
            }
            CompatMode::Xvfb => {
                // Real display via Xvfb; no headless flag so a window renders.
            }
        }
        args.push(format!("--remote-debugging-port={port}"));
        args.push("--remote-debugging-address=127.0.0.1".into());
        // user-data-dir must be =-joined so the path is not treated as a target URL.
        args.push(format!("--user-data-dir={}", temp_dir.to_string_lossy()));
        args.push("--no-first-run".into());
        args.push("--no-default-browser-check".into());
        args.push("--disable-extensions".into());
        args.push("--disable-background-networking".into());
        args.push("--disable-sync".into());
        args.push("--metrics-recording-only".into());
        args.push("--disable-component-update".into());
        args.push("--disable-features=Translate,IsolateOrigins,site-per-process".into());
        args.push("--font-render-hinting=none".into());
        args.push("--hide-scrollbars".into());
        // Graphics: software GL to avoid GPU in headless servers.
        args.push("--disable-gpu".into());
        args.push("--use-gl=angle".into());
        args.push("--use-angle=swiftshader".into());

        if opts.incognito {
            args.push("--incognito".into());
        }
        if opts.no_sandbox {
            args.push("--no-sandbox".into());
        }
        if let Some(proxy) = &opts.proxy {
            args.push(format!("--proxy-server={proxy}"));
        }
        // Window size mirrors viewport so CSS pixels == device pixels at dsf 1.
        args.push(format!(
            "--window-size={},{}",
            opts.viewport.width, opts.viewport.height
        ));
        args.extend(opts.extra_args.iter().cloned());

        // A blank initial target keeps the process alive. Headless=new requires
        // exactly one initial target, so we pass about:blank as the sole URL arg.
        args.push("about:blank".into());

        let mut cmd = Command::new(&exe);
        cmd.args(&args);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        // Propagate env + DISPLAY so the browser can find libs/locale.
        cmd.env_remove("RUST_LOG");
        for (k, v) in &display_env {
            cmd.env(k, v);
        }

        if opts.no_sandbox {
            tracing::warn!(
                "browser launched with --no-sandbox; only do this in trusted/isolated environments"
            );
        }

        let child = cmd
            .spawn()
            .map_err(|e| BrowserError::LaunchFailed(format!("spawn {exe:?}: {e}")))?;

        let proc = Self {
            child: tokio::sync::Mutex::new(Some(child)),
            exe,
            port,
            temp_dir: Some(temp_dir),
            owns_temp,
            xvfb_display,
        };

        // Wait for CDP HTTP to be ready.
        proc.wait_ready().await?;
        Ok(proc)
    }

    /// The CDP HTTP endpoint URL, e.g. `http://127.0.0.1:9222/json/version`.
    pub fn http_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/json/version", self.port)
    }

    /// The CDP HTTP list URL, e.g. `http://127.0.0.1:9222/json`.
    pub fn http_list_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/json", self.port)
    }

    /// Browser executable path.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Query the browser version via CDP HTTP.
    pub async fn version(&self) -> Result<String, BrowserError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;
        let v: Value = client
            .get(self.http_endpoint())
            .send()
            .await
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;
        Ok(v.get("Browser")
            .and_then(|b| b.as_str())
            .unwrap_or("unknown")
            .to_string())
    }

    async fn wait_ready(&self) -> Result<(), BrowserError> {
        let start = Instant::now();
        let deadline = Duration::from_secs(30);
        let url = self.http_endpoint();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;
        loop {
            if start.elapsed() > deadline {
                return Err(BrowserError::LaunchFailed(format!(
                    "CDP endpoint {url} not ready within 30s"
                )));
            }
            // Bail early if the process died.
            {
                let mut g = self.child.lock().await;
                if let Some(child) = g.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(BrowserError::LaunchFailed(format!(
                            "browser exited early with status {status}"
                        )));
                    }
                }
            }
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Kill the process and remove temp dirs. Idempotent.
    pub fn kill_and_cleanup(&self) {
        // start_kill sends SIGKILL immediately (non-async). We then take the child
        // out of the mutex and reap it on a short-lived blocking thread so we don't
        // block the Drop context (which may run inside a tokio runtime) and so we
        // avoid zombies.
        let taken = self.child.try_lock().ok().and_then(|mut g| g.take());
        if let Some(mut child) = taken {
            let _ = child.start_kill();
            // Reap on a dedicated thread; the join is bounded by a tiny timeout.
            // Reap on a dedicated thread with a bounded retry, so a slow shutdown
            // cannot hang Drop indefinitely. SIGKILL makes the process exit fast;
            // we cap waiting at ~2s as a safety net.
            let handle = std::thread::spawn(move || {
                for _ in 0..200 {
                    match child.try_wait() {
                        Ok(Some(_)) => return,
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                        Err(_) => return,
                    }
                }
            });
            let _ = handle.join();
        }
        if self.owns_temp {
            if let Some(d) = &self.temp_dir {
                let _ = std::fs::remove_dir_all(d);
            }
        }
        if let Some(disp) = &self.xvfb_display {
            // Xvfb is left to its own process group; best-effort kill by display.
            let _ = std::process::Command::new("pkill")
                .args(["-f", &format!("Xvfb {disp}")])
                .output();
        }
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        self.kill_and_cleanup();
    }
}

/// Pick a free TCP port by binding to :0.
fn pick_free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok().map(|a| a.port()))
}

/// Start an Xvfb display on a free display number and return `:N`.
async fn start_xvfb() -> Result<String, BrowserError> {
    let n = fastrand::u16(20..200);
    let display = format!(":{n}");
    let _ = Command::new("Xvfb")
        .args([
            display.clone(),
            "-screen".to_string(),
            "0".to_string(),
            "1280x720x24".to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| BrowserError::LaunchFailed(format!("Xvfb spawn: {e}")))?;
    // Give Xvfb a moment to start.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(format!("localhost{display}"))
}

/// Short random id for temp dir names (no uuid dep at call site).
fn short_uuid() -> String {
    let bytes: [u8; 8] = std::array::from_fn(|_| fastrand::u8(0..=255));
    hex::encode(bytes)
}

impl LaunchOptions {
    /// Like [`Self::to_launch_options`] in cli but kept here to avoid a cycle;
    /// returns a default options set for doctor (root-safe).
    pub fn to_launch_options_safe(self) -> Result<Self, BrowserError> {
        Ok(self)
    }

    /// If running as root, enable --no-sandbox automatically with a warning.
    /// This matches the documented Docker/root guidance.
    pub fn with_no_sandbox_for_root(self) -> Self {
        let is_root = is_running_as_root();
        if is_root && !self.no_sandbox {
            tracing::warn!(
                "running as root; enabling --no-sandbox (set --no-sandbox explicitly to silence)"
            );
            Self {
                no_sandbox: true,
                ..self
            }
        } else {
            self
        }
    }
}

impl BrowserProcess {
    /// The CDP port (for display).
    pub fn port_for_display(&self) -> u16 {
        self.port
    }
}

/// Detect root without unsafe code: parse /proc/self/status (Linux) for Uid 0.
fn is_running_as_root() -> bool {
    if cfg!(unix) {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("Uid:") {
                    // "Uid:	0	0	0	0" — first field is real uid.
                    return rest
                        .split_whitespace()
                        .next()
                        .map(|u| u == "0")
                        .unwrap_or(false);
                }
            }
        }
        // Fallback: USER env, best-effort.
        std::env::var("USER").map(|u| u == "root").unwrap_or(false)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_returns_path_or_clear_error() {
        // We can't assert a specific browser, but the error must be typed.
        match discover_browser() {
            Ok(p) => assert!(p.is_file(), "discovered path must be a file"),
            Err(BrowserError::BrowserNotFound(_)) => {}
            Err(e) => panic!("expected BrowserNotFound, got {e:?}"),
        }
    }

    #[test]
    fn which_finds_known_binary() {
        let p = which("sh").or_else(|| which("bash"));
        assert!(p.is_some(), "should find sh or bash on PATH");
    }
}
