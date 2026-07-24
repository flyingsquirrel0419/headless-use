//! CLI command implementations: doctor, launch, serve, run.

use serde_json::json;

use crate::browser::{discover_browser, Browser, LaunchOptions};
use crate::cli::{output, LaunchArgs, RunArgs};
use crate::observe::parse_ref;
use crate::session::Session;

/// Run the `doctor` command: diagnose the environment.
pub async fn doctor() -> i32 {
    let mut checks: Vec<(bool, String, Option<String>)> = Vec::new();

    // OS
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("PRETTY_NAME=")).map(|l| {
                l.trim_start_matches("PRETTY_NAME=")
                    .trim_matches('"')
                    .to_string()
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    checks.push((
        true,
        "OS".into(),
        Some(format!("{os} {}", std::env::consts::ARCH)),
    ));

    // Browser
    match discover_browser() {
        Ok(p) => {
            checks.push((true, "Browser path".into(), Some(p.display().to_string())));
        }
        Err(e) => {
            checks.push((false, "Browser path".into(), Some(e.to_string())));
        }
    }

    // Version + CDP round-trip (best effort).
    let mut browser_ok = false;
    if let Ok(opts) = LaunchOptions::default().to_launch_options_safe() {
        match Browser::launch(opts.with_no_sandbox_for_root()).await {
            Ok(b) => {
                if let Ok(v) = b.process().version().await {
                    checks.push((true, "Browser version".into(), Some(v)));
                }
                // Quick CDP + screenshot test.
                match b.new_page().await {
                    Ok(page) => match page.screenshot(false, None).await {
                        Ok(data) => {
                            let ok = !data.is_empty();
                            checks.push((ok, "CDP connection + screenshot".into(), None));
                            browser_ok = ok;
                        }
                        Err(e) => {
                            checks.push((false, "Screenshot".into(), Some(e.to_string())));
                        }
                    },
                    Err(e) => {
                        checks.push((false, "New page".into(), Some(e.to_string())));
                    }
                }
                b.close().await;
            }
            Err(e) => {
                checks.push((false, "Browser launch".into(), Some(e.to_string())));
            }
        }
    }

    // Keyboard + Korean font (fontconfig).
    let korean = !crate::util::korean_font_available().is_empty();
    checks.push((korean, "Korean font".into(), None));

    // /dev/shm size.
    let shm = std::fs::metadata("/dev/shm").is_ok();
    let shm_size = crate::util::shm_size_mb();
    let shm_ok = shm && shm_size.map(|s| s >= 64).unwrap_or(false);
    let shm_label = shm_size
        .map(|s| format!("{s}MB"))
        .unwrap_or_else(|| "unknown".into());
    checks.push((shm_ok, "/dev/shm".into(), Some(shm_label)));

    // Xvfb presence.
    let xvfb = which_path("Xvfb");
    checks.push((xvfb, "Xvfb (optional)".into(), None));

    // Temp dir writable.
    let tmp_ok = std::env::temp_dir()
        .join("headless-use-doctor")
        .with_extension("tmp");
    let tmp_writable = std::fs::write(&tmp_ok, b"x").is_ok();
    let _ = std::fs::remove_file(&tmp_ok);
    checks.push((tmp_writable, "Temp dir writable".into(), None));

    let _ = browser_ok;
    for (ok, label, detail) in &checks {
        let mark = if *ok { "✓" } else { "✗" };
        match detail {
            Some(d) => println!("{mark} {label}: {d}"),
            None => println!("{mark} {label}"),
        }
    }
    if checks.iter().any(|(ok, _, _)| !*ok) {
        1
    } else {
        0
    }
}

/// Run the `launch` command.
pub async fn launch(args: LaunchArgs) -> i32 {
    let opts = match args.to_launch_options() {
        Ok(o) => o.with_no_sandbox_for_root(),
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    match Browser::launch(opts).await {
        Ok(browser) => {
            let port = browser.process().port_for_display();
            if args.json {
                output::print_json(
                    &json!({ "port": port, "httpEndpoint": browser.process().http_endpoint() }),
                );
            } else {
                println!(
                    "Browser launched. CDP: {}",
                    browser.process().http_endpoint()
                );
                println!("Press Ctrl+C to stop.");
            }
            // Keep alive until interrupted.
            tokio::signal::ctrl_c().await.ok();
            browser.close().await;
            0
        }
        Err(e) => output::print_error(&e),
    }
}

/// Run the `mcp` command: MCP server over stdio.
pub async fn mcp(args: crate::cli::ServeArgs) -> i32 {
    let opts = match args.launch.to_launch_options() {
        Ok(o) => o.with_no_sandbox_for_root(),
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let session = match Session::start(opts).await {
        Ok(s) => s,
        Err(e) => return output::print_error(&e),
    };
    crate::mcp::transport::run(session).await
}

/// Run the `serve` command (JSON-RPC over stdio).
pub async fn serve(args: crate::cli::ServeArgs) -> i32 {
    let opts = match args.launch.to_launch_options() {
        Ok(o) => o.with_no_sandbox_for_root(),
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let session = match Session::start(opts).await {
        Ok(s) => s,
        Err(e) => return output::print_error(&e),
    };
    crate::cli::rpc::run_stdio(session).await
}

/// Run the `run` one-shot command.
pub async fn run(args: RunArgs) -> i32 {
    let opts = match args.launch.to_launch_options() {
        Ok(o) => o.with_no_sandbox_for_root(),
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let session = match Session::start(opts).await {
        Ok(s) => s,
        Err(e) => return output::print_error(&e),
    };
    let result = run_oneshot(&session, &args).await;
    session.shutdown().await;
    match result {
        Ok(()) => 0,
        Err(e) => output::print_error(&e),
    }
}

async fn run_oneshot(
    session: &Session,
    args: &RunArgs,
) -> Result<(), crate::browser::BrowserError> {
    session.open(&args.url).await?;
    session.wait(Default::default()).await?;
    if let Some(path) = &args.screenshot {
        let data = session.screenshot(args.full_page).await?;
        crate::util::write_bytes(std::path::Path::new(path), &data)
            .map_err(crate::browser::BrowserError::Io)?;
        if args.launch.json {
            output::print_json(&json!({ "screenshot": path, "bytes": data.len() }));
        } else {
            println!("Screenshot saved to {path} ({} bytes)", data.len());
        }
    }
    Ok(())
}

/// Parse a reference like `@e3` (returns Err to hint caller to re-observe).
pub fn parse_target_ref(s: &str) -> Result<u32, String> {
    parse_ref(s)
}

fn which_path(name: &str) -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

/// Run the `install-browser` command (best-effort: prints guidance).
pub async fn install_browser() -> i32 {
    println!("Automatic browser download is not yet implemented.");
    println!("Install chrome-headless-shell or Chromium via your package manager:");
    println!();
    println!("  # Debian/Ubuntu");
    println!("  apt-get update && apt-get install -y chromium-browser");
    println!();
    println!("  # Or download chrome-headless-shell from:");
    println!("  https://googlechromelabs.github.io/chrome-for-testing/");
    println!();
    println!("Then set HEADLESS_USE_BROWSER_PATH or use --browser-path.");
    println!();
    println!("For Docker, see docker/Dockerfile which bundles the browser.");
    0
}
