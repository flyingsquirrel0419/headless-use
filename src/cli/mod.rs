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
    /// Viewer HTTP port (binds to 127.0.0.1 only).
    #[arg(long, default_value_t = 7780)]
    pub viewer_port: u16,
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
}

/// Build a navigation policy from allow/deny host lists.
/// When both lists are empty the policy is permissive (allows everything).
pub fn build_policy(allow: &[String], deny: &[String]) -> crate::security::Policy {
    crate::security::Policy {
        allow_hosts: allow.to_vec(),
        deny_hosts: deny.to_vec(),
    }
}
