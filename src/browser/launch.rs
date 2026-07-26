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
    /// Suppress the signals that identify `--headless=new` as automated, so bot
    /// checks (Cloudflare Turnstile and friends) see an ordinary Chrome. Costs
    /// nothing at runtime and keeps headless — see [`crate::browser::stealth`]
    /// for what it changes and why, including the two launch defaults it drops.
    pub stealth: bool,
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
            stealth: false,
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
    discover_browser_with(false)
}

/// Discover a browser executable, optionally skipping the headless shells.
///
/// `chrome-headless-shell` is a stripped build: no `window.chrome`, no PDF
/// plugin entries, no proprietary codecs, and a product name that says
/// `HeadlessChrome`. Those are exactly the properties a bot check reads, and
/// none of them can be restored from JS convincingly — so stealth mode asks for
/// a full browser and only falls back to a shell if that is all there is.
pub fn discover_browser_with(prefer_full_browser: bool) -> Result<PathBuf, BrowserError> {
    if let Ok(p) = std::env::var("HEADLESS_USE_BROWSER_PATH") {
        let path = PathBuf::from(&p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(BrowserError::BrowserNotFound(format!(
            "HEADLESS_USE_BROWSER_PATH={p} does not exist"
        )));
    }
    let is_shell = |name: &str| name.contains("headless-shell");
    if prefer_full_browser {
        for name in BROWSER_CANDIDATES.iter().filter(|n| !is_shell(n)) {
            if let Some(p) = which(name) {
                return Ok(p);
            }
        }
        if let Some(p) = BROWSER_CANDIDATES
            .iter()
            .filter(|n| is_shell(n))
            .find_map(|n| which(n))
        {
            tracing::warn!(
                exe = %p.display(),
                "stealth: only a headless shell was found; it is missing browser APIs \
                 (window.chrome, PDF plugins, proprietary codecs) that bot checks read. \
                 Install full Chrome/Chromium for reliable stealth."
            );
            return Ok(p);
        }
    } else {
        for name in BROWSER_CANDIDATES {
            if let Some(p) = which(name) {
                return Ok(p);
            }
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
    /// A `std::sync::Mutex`, not a tokio one, because every operation on the
    /// child (`try_wait`, `start_kill`) is synchronous and the guard is never
    /// held across an await. That lets cleanup take the lock unconditionally
    /// instead of `try_lock`-ing it: the old code silently skipped the kill on
    /// contention, leaking the browser process and its temp profile.
    child: std::sync::Mutex<Option<Child>>,
    exe: PathBuf,
    port: u16,
    temp_dir: Option<PathBuf>,
    owns_temp: bool,
    xvfb_display: Option<String>,
    /// PID of the Xvfb server we started, so cleanup kills exactly that one.
    xvfb_pid: Option<u32>,
    /// Resolved stealth identity when launched with `stealth: true`. Held here
    /// because the per-page half of stealth (UA override, pre-load script) needs
    /// the same version-derived values the launch flags were built from.
    stealth: Option<crate::browser::stealth::StealthProfile>,
    /// Handle into the process-wide cleanup registry (see
    /// [`install_process_guards`]), removed once this process cleans itself up.
    cleanup_id: u64,
    /// Claims on the auto-picked CDP port and Xvfb display, held for the life of
    /// the process so no concurrent launch in this process reuses either. Never
    /// read; the value is the `Drop`.
    _reservations: Vec<Reservation>,
}

impl BrowserProcess {
    /// Launch Chrome and wait until the CDP HTTP endpoint responds.
    ///
    /// With `opts.port == 0` the port is auto-picked, and a launch that fails
    /// the way a lost port race fails (the browser exits within seconds of
    /// spawning, because it could not open the debugging port) is retried with a
    /// fresh port. Retries are deliberately limited to that fast failure: a
    /// browser that spawns but never answers burns the full 30s readiness
    /// deadline, and retrying *that* would turn one 30s failure into four.
    pub async fn launch(opts: LaunchOptions) -> Result<Self, BrowserError> {
        if opts.port != 0 {
            return Self::launch_once(&opts, None).await;
        }
        const PORT_ATTEMPTS: u32 = 4;
        let mut last: Option<BrowserError> = None;
        for attempt in 1..=PORT_ATTEMPTS {
            let reservation = reserve_free_port()
                .ok_or_else(|| BrowserError::LaunchFailed("no free port".into()))?;
            let port = reservation.value;
            match Self::launch_once(&opts, Some(reservation)).await {
                Ok(proc) => return Ok(proc),
                Err(e) if is_port_race_failure(&e) && attempt < PORT_ATTEMPTS => {
                    // Debug, not warn. "Exited early" is also how a browser with
                    // a missing shared library or an unwritable $HOME fails, and
                    // at the default log level three warnings blaming the port
                    // are worse than silence: they point at the wrong cause for
                    // the most common broken-environment case. If every attempt
                    // fails, the last real error is returned and `doctor`
                    // explains it.
                    tracing::debug!(
                        port,
                        attempt,
                        error = %e,
                        "browser launch failed in the way a taken debugging port fails; \
                         re-picking a port and retrying"
                    );
                    last = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| BrowserError::LaunchFailed("no free port".into())))
    }

    /// One launch attempt on an already-chosen port.
    async fn launch_once(
        opts: &LaunchOptions,
        port_reservation: Option<Reservation>,
    ) -> Result<Self, BrowserError> {
        let exe = match opts.browser_path.clone() {
            Some(p) if p.is_file() => p,
            _ => discover_browser_with(opts.stealth)?,
        };

        // Built before the flag list because the UA flag comes from it.
        let stealth = if opts.stealth {
            Some(crate::browser::stealth::StealthProfile::detect(&exe).await)
        } else {
            None
        };

        let port = match &port_reservation {
            Some(r) => r.value,
            None => opts.port,
        };
        let auto_port = port_reservation.is_some();
        let mut reservations: Vec<Reservation> = port_reservation.into_iter().collect();

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
        let (xvfb_display, xvfb_pid) = match opts.compat {
            CompatMode::Xvfb => {
                let (d, pid, reservation) = start_xvfb().await?;
                reservations.push(reservation);
                (Some(d), Some(pid))
            }
            CompatMode::Chromium => (None, None),
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
        // Only Translate is disabled here. `IsolateOrigins` and
        // `site-per-process` used to be in this list; turning them off disables
        // Chrome's site isolation, which is the main defense between a page and
        // the rest of the browser — a bad trade on a browser an agent points at
        // arbitrary URLs. Site isolation costs memory, not correctness, so it
        // stays on.
        args.push("--disable-features=Translate".into());
        args.push("--font-render-hinting=none".into());
        // Scrollbars are hidden so screenshots match the CSS viewport — but a
        // zero-width scrollbar shows up as `innerWidth == clientWidth`, which is
        // a documented headless check, so stealth keeps them.
        if !opts.stealth {
            args.push("--hide-scrollbars".into());
        }
        // Graphics: software GL to avoid GPU in headless servers.
        // `--disable-gpu` shuts the GPU process down entirely, which also takes
        // WebGL with it; a page with no WebGL context at all is a louder signal
        // than a software renderer (whose driver strings stealth.js rewrites).
        // So stealth keeps ANGLE/SwiftShader and skips --disable-gpu.
        if !opts.stealth {
            args.push("--disable-gpu".into());
        }
        args.push("--use-gl=angle".into());
        args.push("--use-angle=swiftshader".into());
        if let Some(profile) = &stealth {
            args.extend(profile.launch_args());
        }

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

        // Last check before the child takes the port: the probe in
        // `reserve_free_port` happened before the temp dir, Xvfb and the flag
        // list were built, which is plenty of time for another process to bind
        // it. Re-checking here shrinks the window to the microseconds between
        // this bind and Chrome's own; it does not close it (see
        // `reserve_free_port`), so `launch` also retries.
        if auto_port && !port_is_free(port) {
            // Nothing is registered for cleanup yet, so undo by hand before the
            // retry: otherwise every lost race leaks a profile dir and an X
            // server.
            if owns_temp {
                remove_dir_with_retry(&temp_dir, 3);
            }
            if let Some(pid) = xvfb_pid {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
            }
            return Err(BrowserError::LaunchFailed(format!(
                "debugging port {port} was taken between selection and launch"
            )));
        }

        let child = cmd
            .spawn()
            .map_err(|e| BrowserError::LaunchFailed(format!("spawn {exe:?}: {e}")))?;

        // Register before anything can fail, so a launch that dies during
        // `wait_ready` still gets its process and profile dir reclaimed.
        let cleanup_id =
            register_cleanup(child.id(), owns_temp.then(|| temp_dir.clone()), xvfb_pid);

        let proc = Self {
            child: std::sync::Mutex::new(Some(child)),
            exe,
            port,
            temp_dir: Some(temp_dir),
            owns_temp,
            xvfb_display,
            xvfb_pid,
            stealth,
            cleanup_id,
            _reservations: reservations,
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
            // Bail early if the process died. The guard is scoped so it is
            // never held across the await below.
            {
                let mut g = self.child.lock().unwrap_or_else(|e| e.into_inner());
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
        unregister_cleanup(self.cleanup_id);
        // start_kill sends SIGKILL immediately (non-async). We then take the child
        // out of the mutex and reap it on a short-lived blocking thread so we don't
        // block the Drop context (which may run inside a tokio runtime) and so we
        // avoid zombies.
        let taken = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
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
                // Retried for the same reason as in `cleanup_all_spawned`:
                // Chrome's helper processes may still be flushing into the
                // profile when the main process is reaped, and a single
                // `remove_dir_all` then fails with ENOTEMPTY and leaves the
                // directory behind for good.
                remove_dir_with_retry(d, 5);
            }
        }
        if let Some(pid) = self.xvfb_pid {
            // Kill by pid, not `pkill -f "Xvfb :N"` — the pattern match would
            // also take down an Xvfb started by someone else on the same host.
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
        }
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        self.kill_and_cleanup();
    }
}

/// What has to be cleaned up for one spawned browser, as plain data.
///
/// Deliberately not a handle to [`BrowserProcess`]: the signal handler and the
/// panic hook run on threads that own nothing, and may run while another thread
/// holds the process mutex. Plain pids and paths can always be acted on.
#[derive(Debug, Clone)]
struct CleanupRecord {
    id: u64,
    chrome_pid: Option<u32>,
    temp_dir: Option<PathBuf>,
    xvfb_pid: Option<u32>,
}

/// Everything this process has spawned and not yet cleaned up.
static SPAWNED: std::sync::Mutex<Vec<CleanupRecord>> = std::sync::Mutex::new(Vec::new());
static NEXT_CLEANUP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn register_cleanup(
    chrome_pid: Option<u32>,
    temp_dir: Option<PathBuf>,
    xvfb_pid: Option<u32>,
) -> u64 {
    let id = NEXT_CLEANUP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = SPAWNED.lock() {
        g.push(CleanupRecord {
            id,
            chrome_pid,
            temp_dir,
            xvfb_pid,
        });
    }
    id
}

fn unregister_cleanup(id: u64) {
    if let Ok(mut g) = SPAWNED.lock() {
        g.retain(|r| r.id != id);
    }
}

/// Kill every browser this process spawned and remove its temp profile dirs.
///
/// Safe to call from a signal handler task or a panic hook, and safe to call
/// more than once.
pub fn cleanup_all_spawned() {
    let records = match SPAWNED.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    for r in records {
        if let Some(pid) = r.chrome_pid {
            // `kill` the binary rather than libc: this crate is
            // `#![forbid(unsafe_code)]`, and the process is going away anyway.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
            wait_for_exit(pid, std::time::Duration::from_secs(2));
        }
        if let Some(pid) = r.xvfb_pid {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
        }
        if let Some(d) = r.temp_dir {
            // Removal is retried, and the failure is reported rather than
            // swallowed. SIGKILL is delivered asynchronously: deleting the
            // profile the instant after signalling races Chrome's own writers
            // and fails with ENOTEMPTY, which is how the temp dir survived
            // every SIGTERM while the process itself was correctly killed.
            remove_dir_with_retry(&d, 5);
        }
    }
}

/// Block until `pid` is gone, or `budget` elapses.
///
/// Reads `/proc/<pid>` rather than `waitpid`: the caller is a signal-handler
/// task or a panic hook that does not own the child handle.
fn wait_for_exit(pid: u32, budget: std::time::Duration) {
    let deadline = Instant::now() + budget;
    let proc_entry = PathBuf::from(format!("/proc/{pid}"));
    while proc_entry.exists() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Remove a directory tree, retrying while a dying process still writes to it.
fn remove_dir_with_retry(dir: &Path, attempts: u32) {
    for attempt in 0..attempts {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt + 1 == attempts => {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "could not remove temp profile directory; it will need manual cleanup"
                );
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// Install process-wide cleanup for signals and panics.
///
/// ## Why this exists
/// Cleanup used to live entirely in `Drop`. `Drop` does not run when the
/// process is killed by SIGTERM (every container stop, every CI cancel, every
/// supervisor restart), and it does not run on panic either, because the
/// release profile sets `panic = "abort"`. Both cases leaked a Chrome process
/// and a temp profile directory — on a CI box that is a slow disk-fill.
///
/// Only `launch` installed a `ctrl_c` handler; `serve`, `mcp`, `view` and `run`
/// had none, which is precisely backwards, since those are the long-lived ones.
///
/// Idempotent: repeated calls install the hooks once.
pub fn install_process_guards() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            cleanup_all_spawned();
            previous(info);
        }));

        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            tokio::spawn(async move {
                let mut term = match signal(SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot listen for SIGTERM");
                        return;
                    }
                };
                let mut int = match signal(SignalKind::interrupt()) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot listen for SIGINT");
                        return;
                    }
                };
                let (code, name) = tokio::select! {
                    _ = term.recv() => (143, "SIGTERM"),
                    _ = int.recv() => (130, "SIGINT"),
                };
                tracing::info!(
                    signal = name,
                    "shutting down; cleaning up browser processes"
                );
                cleanup_all_spawned();
                std::process::exit(code);
            });
        }
    });
}

/// Does this launch failure look like the debugging port was taken by someone
/// else after we picked it?
///
/// Two shapes: the pre-spawn re-check caught it, or Chrome could not open the
/// port and exited on its own within the readiness poll. A readiness *timeout*
/// is not included — the browser is alive and something else is wrong, and
/// retrying would cost another 30s per attempt.
///
/// "Exited early" is not exclusive to a port collision (a browser missing a
/// shared library exits early too), so a genuinely broken install is retried a
/// few times before failing. That costs a handful of sub-second spawns, and the
/// last attempt's real error is what the caller sees — the diagnostic is not
/// swallowed.
fn is_port_race_failure(e: &BrowserError) -> bool {
    match e {
        BrowserError::LaunchFailed(msg) => {
            msg.contains("was taken between selection and launch") || msg.contains("exited early")
        }
        _ => false,
    }
}

/// Numbers this process has claimed for a not-yet-started child.
///
/// ## Why a process-wide claim, not just a probe
/// Both resources below are picked by *probing* — bind `127.0.0.1:0` and read
/// the port back, or look for an X socket that does not exist yet. Neither
/// probe holds the resource: the listener has to be closed before Chrome can
/// bind the same port, and the X socket only appears once Xvfb starts. So two
/// launches racing inside one process (exactly what `cargo test
/// --test-threads=2` does) could probe in the gap and be handed the same number,
/// and the loser died with an unhelpful launch error.
///
/// A claim list removes the in-process half of the race outright: a number
/// handed out here is not handed out again until the [`Reservation`] is
/// dropped. It cannot remove the cross-process half — see [`reserve_free_port`].
static RESERVED_PORTS: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
static RESERVED_DISPLAYS: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

/// A claimed port or display number, released on drop.
struct Reservation {
    value: u16,
    set: &'static std::sync::Mutex<Vec<u16>>,
}

impl Reservation {
    /// Claim `value` unless this process already handed it out.
    fn try_claim(set: &'static std::sync::Mutex<Vec<u16>>, value: u16) -> Option<Self> {
        let mut g = set.lock().unwrap_or_else(|e| e.into_inner());
        if g.contains(&value) {
            return None;
        }
        g.push(value);
        Some(Self { value, set })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut g = self.set.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|v| *v != self.value);
    }
}

/// Probe for a free TCP port by binding `127.0.0.1:0` and reading it back.
///
/// The listener is closed on return — it has to be, or Chrome could not bind
/// the port itself. Use [`reserve_free_port`], which layers the process-wide
/// claim and a re-check on top.
fn probe_free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok().map(|a| a.port()))
}

/// Can we still bind `port` right now?
///
/// Called immediately before spawning Chrome to catch a port that was taken
/// since it was probed. The listener is dropped at the end of the expression so
/// the port is free again for the child.
fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Reserve a port for a browser we are about to launch.
///
/// ## Residual race (not eliminated, only narrowed)
/// Between this function releasing its probe listener and Chrome binding the
/// port, any *other process* on the host can take it — the kernel offers no way
/// to hand a bound listener to a child that wants to bind it itself. Two things
/// narrow the window: the claim list (no second launch *in this process* can be
/// given the same number), and a `port_is_free` re-check immediately before
/// spawn. What is left is a genuine cross-process TOCTOU; [`BrowserProcess::launch`]
/// therefore also retries the whole launch with a fresh port when the browser
/// dies immediately, which is how a lost race presents.
fn reserve_free_port() -> Option<Reservation> {
    for _ in 0..64 {
        let port = probe_free_port()?;
        if let Some(reservation) = Reservation::try_claim(&RESERVED_PORTS, port) {
            return Some(reservation);
        }
        // Claimed by a concurrent launch in this process; probe again.
    }
    None
}

/// Reserve a display number no X server is currently using.
///
/// An X server on display `:N` holds `/tmp/.X11-unix/XN` and `/tmp/.X<N>-lock`.
/// Picking at random without checking (the original behavior) could collide
/// with an existing server — including one started by another `headless-use`
/// session — and the Xvfb spawn would fail silently.
///
/// Same residual race as [`reserve_free_port`], for the same reason: the files
/// only appear once Xvfb has started, so a *different process* can still claim
/// `:N` in between. In-process collisions are impossible thanks to the claim.
fn reserve_free_display() -> Option<Reservation> {
    for _ in 0..64 {
        let n = fastrand::u16(20..200);
        let socket = std::path::Path::new("/tmp/.X11-unix").join(format!("X{n}"));
        let lock = std::path::PathBuf::from(format!("/tmp/.X{n}-lock"));
        if socket.exists() || lock.exists() {
            continue;
        }
        if let Some(reservation) = Reservation::try_claim(&RESERVED_DISPLAYS, n) {
            return Some(reservation);
        }
    }
    None
}

/// Start an Xvfb display on a free display number and return `:N`.
///
/// The [`Reservation`] is returned with it and must be held for as long as the
/// server runs, so no other launch in this process reuses the number.
async fn start_xvfb() -> Result<(String, u32, Reservation), BrowserError> {
    let reservation = reserve_free_display()
        .ok_or_else(|| BrowserError::LaunchFailed("no free X display number".into()))?;
    let n = reservation.value;
    let display = format!(":{n}");
    let child = Command::new("Xvfb")
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
    // Keep the pid so cleanup can kill exactly this server. The old cleanup
    // ran `pkill -f "Xvfb :N"`, which would also kill an unrelated Xvfb that
    // happened to share the display string.
    let pid = child
        .id()
        .ok_or_else(|| BrowserError::LaunchFailed("Xvfb exited immediately".into()))?;
    // Hold the child so it is reaped rather than becoming a zombie; the process
    // itself is killed by pid during cleanup.
    std::mem::forget(child);

    // Wait for the X server to actually be listening instead of sleeping a
    // fixed 300ms and hoping. Xvfb creates its unix socket once it is ready to
    // accept clients, so the socket is the readiness signal. A fixed sleep is
    // both too long on a fast machine and too short on a loaded CI runner,
    // where Chrome then fails with an unhelpful "browser exited early".
    let socket = std::path::Path::new("/tmp/.X11-unix").join(format!("X{n}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if socket.exists() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
            return Err(BrowserError::LaunchFailed(format!(
                "Xvfb on {display} did not become ready within 10s"
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Return the bare `:N` form. `localhost:N` makes the X client use an
    // abstract/TCP transport path that some Chrome builds reject on headless
    // servers, causing "browser exited early". The bare display number (e.g.
    // `:99`) uses the standard X11 unix socket and matches a manual
    // `DISPLAY=:99 google-chrome` invocation.
    Ok((display, pid, reservation))
}

/// Short random id for temp dir names (no uuid dep at call site).
fn short_uuid() -> String {
    let bytes: [u8; 8] = std::array::from_fn(|_| fastrand::u8(0..=255));
    hex::encode(bytes)
}

impl LaunchOptions {
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

    /// The Xvfb display this process runs on (`:N`), if `--compat xvfb`.
    pub fn xvfb_display(&self) -> Option<&str> {
        self.xvfb_display.as_deref()
    }

    /// The stealth identity this process was launched with, if any.
    pub fn stealth(&self) -> Option<&crate::browser::stealth::StealthProfile> {
        self.stealth.as_ref()
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
