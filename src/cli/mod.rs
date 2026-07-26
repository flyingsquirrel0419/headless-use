//! CLI definition and dispatch.
//!
//! The CLI is designed for AI agents: short, predictable output, `--json` on
//! every command, clear exit codes, stdout/stderr separation, and recovery
//! hints in errors.

pub mod commands;
pub mod output;
pub mod rpc;

use clap::{Parser, Subcommand};

use crate::browser::LaunchOptions;
use crate::cdp::Viewport;

/// `headless-use`: Computer use for web development agents, built for headless Linux and CI.
#[derive(Parser, Debug)]
#[command(name = "headless-use", version, about, long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Launch a browser and keep it running (prints the port).
    Launch(LaunchArgs),
    /// Start a long-lived session server (JSON-RPC over stdio).
    Serve(ServeArgs),
    /// Run a one-shot action and exit.
    Run(RunArgs),
    /// Diagnose the environment.
    Doctor,
    /// Download/install a browser (best-effort).
    InstallBrowser,
    /// Start the MCP (Model Context Protocol) server over stdio.
    Mcp(ServeArgs),
    /// Replay a recorded trace from a run directory.
    Replay(ReplayArgs),
    /// Start a session with a live localhost viewer (MJPG stream + cursor overlay).
    View(ViewArgs),
    /// Capture an animated text region and reverse per-glyph wobble (pixels only).
    Dewiggle(DewiggleArgs),
}

/// Arguments shared by launch/serve/open.
#[derive(Parser, Debug, Clone)]
pub struct LaunchArgs {
    /// Viewport size, e.g. `1280x720`.
    #[arg(long, default_value = "1280x720")]
    pub viewport: String,
    /// Device scale factor.
    #[arg(long, default_value_t = 1.0)]
    pub device_scale_factor: f64,
    /// Persistent user-data directory. If omitted, a temp dir is used.
    #[arg(long)]
    pub user_data_dir: Option<String>,
    /// Incognito mode.
    #[arg(long)]
    pub incognito: bool,
    /// HTTP proxy URL.
    #[arg(long)]
    pub proxy: Option<String>,
    /// Explicit browser executable path.
    #[arg(long)]
    pub browser_path: Option<String>,
    /// Compatibility mode.
    #[arg(long, default_value = "chromium")]
    pub compat: String,
    /// Disable the sandbox (required as root; use only in trusted envs).
    #[arg(long)]
    pub no_sandbox: bool,
    /// CDP port. 0 = auto.
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    /// Output JSON.
    #[arg(long)]
    pub json: bool,
}

impl LaunchArgs {
    /// Convert to LaunchOptions.
    pub fn to_launch_options(&self) -> Result<LaunchOptions, String> {
        let viewport = Viewport::parse(&self.viewport)?;
        Ok(LaunchOptions {
            browser_path: self.browser_path.as_ref().map(std::path::PathBuf::from),
            viewport: Viewport {
                width: viewport.width,
                height: viewport.height,
                device_scale_factor: self.device_scale_factor,
            },
            user_data_dir: self.user_data_dir.as_ref().map(std::path::PathBuf::from),
            incognito: self.incognito,
            proxy: self.proxy.clone(),
            compat: parse_compat(&self.compat)?,
            no_sandbox: self.no_sandbox,
            extra_args: Vec::new(),
            port: self.port,
        })
    }
}

/// Serve args.
#[derive(Parser, Debug, Clone)]
pub struct ServeArgs {
    /// Browser launch options.
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Restrict navigation to these hosts (repeatable). Others are blocked.
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_hosts: Vec<String>,
    /// Always block navigation to these hosts (repeatable, takes precedence).
    #[arg(long = "deny-host", value_name = "HOST")]
    pub deny_hosts: Vec<String>,
    /// Cursor travel to click/hover targets: `instant` (default here) or
    /// `smooth`. Smooth emits intermediate mouse-move events, which costs
    /// travel time per click but drives hover-dependent UI and looks natural
    /// in a viewer.
    #[arg(long, default_value = "instant")]
    pub cursor_motion: String,
}

/// Run args (one-shot).
#[derive(Parser, Debug, Clone)]
pub struct RunArgs {
    /// URL to open.
    #[arg(long)]
    pub url: String,
    /// Save a screenshot to this path.
    #[arg(long)]
    pub screenshot: Option<String>,
    /// Full-page screenshot.
    #[arg(long)]
    pub full_page: bool,
    /// Annotate the screenshot with bounding boxes + ref tokens for every
    /// observed element (interactive in green, visual widgets in orange).
    #[arg(long)]
    pub annotate: bool,
    /// Screenshot only this element's region by reference (e.g. @g1:e3).
    #[arg(long)]
    pub element: Option<String>,
    /// Browser launch options.
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Restrict navigation to these hosts (repeatable). Others are blocked.
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_hosts: Vec<String>,
    /// Always block navigation to these hosts (repeatable, takes precedence).
    #[arg(long = "deny-host", value_name = "HOST")]
    pub deny_hosts: Vec<String>,
}

/// Dewiggle args: capture an animated text region and reverse per-glyph
/// vertical wobble using pixels only. Honest pixel-only approach — no answer
/// arrays or DOM text are read.
#[derive(Parser, Debug, Clone)]
pub struct DewiggleArgs {
    /// URL to open first.
    #[arg(long)]
    pub url: String,
    /// Capture region as x,y,w,h in viewport CSS px. When omitted, the canvas
    /// element's bounding box is auto-detected.
    #[arg(long, value_name = "X,Y,W,H")]
    pub region: Option<String>,
    /// Number of frames to capture (2..=60, default 12).
    #[arg(long, default_value_t = 12)]
    pub frames: u32,
    /// Milliseconds between frames (default 80).
    #[arg(long, default_value_t = 80)]
    pub interval: u64,
    /// Segment into N equal-width glyph bands and save per-glyph crops.
    #[arg(long)]
    pub chars: Option<usize>,
    /// Save the realigned PNG here.
    #[arg(long, value_name = "PATH")]
    pub out: String,
    /// Browser launch options.
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Restrict navigation to these hosts (repeatable).
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_hosts: Vec<String>,
    /// Always block navigation to these hosts (repeatable, takes precedence).
    #[arg(long = "deny-host", value_name = "HOST")]
    pub deny_hosts: Vec<String>,
}

/// Parse compatibility mode.
fn parse_compat(s: &str) -> Result<crate::browser::launch::CompatMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "chromium" => Ok(crate::browser::launch::CompatMode::Chromium),
        "xvfb" => Ok(crate::browser::launch::CompatMode::Xvfb),
        other => Err(format!("unknown compat '{other}' (chromium|xvfb)")),
    }
}

/// Replay args: re-execute a recorded trace.
#[derive(Parser, Debug, Clone)]
pub struct ReplayArgs {
    /// The run directory containing actions.jsonl (e.g. .headless-use/runs/<ts>-<id>/).
    pub run_dir: String,
    /// Browser launch options.
    #[command(flatten)]
    pub launch: LaunchArgs,
}

/// View args: launch a session with a live localhost MJPEG viewer.
#[derive(Parser, Debug, Clone)]
pub struct ViewArgs {
    /// Browser launch options.
    #[command(flatten)]
    pub launch: LaunchArgs,
    /// Viewer HTTP port.
    #[arg(long, default_value_t = 7780)]
    pub viewer_port: u16,
    /// Viewer bind address. Default 127.0.0.1 (loopback only). Use 0.0.0.0 to
    /// expose the viewer on all interfaces for remote viewing.
    #[arg(long, default_value = "127.0.0.1")]
    pub viewer_host: String,
    /// JPEG quality (1..100) for the screencast stream.
    #[arg(long, default_value_t = 80)]
    pub quality: u32,
    /// Target framerate cap (everyNthFrame is always 1; this scales max dims).
    #[arg(long, default_value_t = 30)]
    pub fps: u32,
    /// Restrict navigation to these hosts (repeatable). Others are blocked.
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_hosts: Vec<String>,
    /// Always block navigation to these hosts (repeatable, takes precedence).
    #[arg(long = "deny-host", value_name = "HOST")]
    pub deny_hosts: Vec<String>,
    /// Cursor travel to click/hover targets: `smooth` (default here) or
    /// `instant`. `view` exists to be watched, so the cursor walks to its
    /// target by default.
    #[arg(long, default_value = "smooth")]
    pub cursor_motion: String,
}

/// Build a navigation policy from allow/deny host lists.
/// When both lists are empty the policy is permissive (allows everything).
pub fn build_policy(allow: &[String], deny: &[String]) -> crate::security::Policy {
    crate::security::Policy {
        allow_hosts: allow.to_vec(),
        deny_hosts: deny.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::CursorMotion;

    fn parse_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args should parse")
    }

    /// `view` exists to be watched, so its cursor walks to the target.
    /// `serve` drives automation, so it stays instant. Getting these backwards
    /// would silently add ~220ms to every click in CI.
    #[test]
    fn cursor_motion_defaults_differ_per_command() {
        let cli = parse_from(&["headless-use", "view"]);
        let Command::View(view) = cli.command else {
            panic!("expected View");
        };
        assert_eq!(
            CursorMotion::parse(&view.cursor_motion).unwrap(),
            CursorMotion::smooth_default(),
            "`view` must default to smooth"
        );

        let cli = parse_from(&["headless-use", "serve"]);
        let Command::Serve(serve) = cli.command else {
            panic!("expected Serve");
        };
        assert_eq!(
            CursorMotion::parse(&serve.cursor_motion).unwrap(),
            CursorMotion::Instant,
            "`serve` must default to instant"
        );
    }

    #[test]
    fn cursor_motion_flag_overrides_the_default() {
        let cli = parse_from(&["headless-use", "view", "--cursor-motion", "instant"]);
        let Command::View(view) = cli.command else {
            panic!("expected View");
        };
        assert_eq!(
            CursorMotion::parse(&view.cursor_motion).unwrap(),
            CursorMotion::Instant
        );

        let cli = parse_from(&["headless-use", "serve", "--cursor-motion", "smooth"]);
        let Command::Serve(serve) = cli.command else {
            panic!("expected Serve");
        };
        assert!(CursorMotion::parse(&serve.cursor_motion)
            .unwrap()
            .is_smooth());
    }
}
