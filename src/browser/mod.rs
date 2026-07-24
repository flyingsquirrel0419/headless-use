//! Browser process management: discovery, launch, lifecycle.
//!
//! [`Browser`] owns the Chrome process and the browser-level CDP connection.
//! Pages are created via [`Browser::new_page`] which returns a [`Page`] bound to
//! a target session.
//!
//! ## Why direct process control
//! We spawn Chrome ourselves (rather than via Playwright/CDP-from-a-driver) so
//! we own cleanup: temp user-data-dir deletion, zombie prevention, and graceful
//! shutdown on SIGINT/SIGTERM. We attach over CDP only after the process is up.

pub mod error;
pub mod launch;
pub mod page;

pub use error::BrowserError;
pub use launch::{discover_browser, BrowserProcess, LaunchOptions};
pub use page::Page;

use std::sync::Arc;

use crate::cdp::CdpClient;

/// A running browser: the process handle + browser-level CDP client.
///
/// Dropping [`Browser`] terminates the Chrome process and cleans temp dirs.
pub struct Browser {
    proc: Arc<BrowserProcess>,
    client: CdpClient,
}

impl Browser {
    /// Launch a browser per `opts` and connect to its browser-level CDP endpoint.
    pub async fn launch(opts: LaunchOptions) -> Result<Self, BrowserError> {
        let proc = BrowserProcess::launch(opts).await?;
        let ws_url = crate::cdp::client::browser_ws_url(&proc.http_endpoint())
            .await
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;
        let client = CdpClient::connect(&ws_url)
            .await
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;
        Ok(Self {
            proc: Arc::new(proc),
            client,
        })
    }

    /// The browser-level CDP client (used for target management).
    pub fn cdp(&self) -> &CdpClient {
        &self.client
    }

    /// The underlying process handle.
    pub fn process(&self) -> &BrowserProcess {
        &self.proc
    }

    /// Create a new tab/page and return a [`Page`] bound to its session.
    pub async fn new_page(&self) -> Result<Page, BrowserError> {
        Page::create(self).await
    }

    /// Create a new tab and immediately navigate to `url`.
    pub async fn open(&self, url: &str) -> Result<Page, BrowserError> {
        let page = self.new_page().await?;
        page.goto(url).await?;
        Ok(page)
    }

    /// Gracefully close: close all targets, kill the process, remove temp dir.
    pub async fn close(self) {
        let _ = self
            .client
            .call::<serde_json::Value>("Browser.close", None, std::time::Duration::from_secs(5))
            .await;
        drop(self.client);
        if let Ok(p) = Arc::try_unwrap(self.proc) {
            p.kill_and_cleanup();
        }
    }

    /// Best-effort shutdown when the Browser is behind an Arc (shared session).
    /// Sends Browser.close and kills the process; safe to call multiple times.
    pub async fn shutdown(&self) {
        let _ = self
            .client
            .call::<serde_json::Value>("Browser.close", None, std::time::Duration::from_secs(5))
            .await;
        // BrowserProcess::drop kills on the last Arc reference; force kill now.
        self.proc.kill_and_cleanup();
    }
}
