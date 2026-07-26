//! Console + page error collection.
//!
//! The collector script is injected via `Page.addScriptToEvaluateOnNewDocument`
//! at page creation (see [`crate::browser::Page::create`]), so it runs before any
//! page JS and captures all console calls, errors, and unhandled rejections.

use serde_json::Value;

use crate::browser::{BrowserError, Page};
use crate::util::secrets;

/// Console message severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ConsoleLevel {
    /// verbose/debug.
    Debug,
    /// info/log.
    Info,
    /// warning.
    Warning,
    /// error.
    Error,
}

impl ConsoleLevel {
    /// Parse from a CDP/string.
    pub fn from_cdp(s: &str) -> Self {
        match s {
            "verbose" | "debug" => ConsoleLevel::Debug,
            "info" | "log" => ConsoleLevel::Info,
            "warning" | "warn" => ConsoleLevel::Warning,
            "error" => ConsoleLevel::Error,
            _ => ConsoleLevel::Info,
        }
    }
}

/// One console entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleEntry {
    /// Severity.
    pub level: ConsoleLevel,
    /// Message text.
    pub text: String,
    /// Source URL (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Line number (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
}

/// Read the buffered console entries from the injected collector.
pub async fn collect(page: &Page) -> Result<Vec<ConsoleEntry>, BrowserError> {
    let expr = r#"
    (() => {
      const out = (window.__hu_console__ || []).map(e => ({
        level: e.level, text: e.text, url: e.url || null, line: e.line || null
      }));
      return JSON.stringify(out);
    })()
    "#;
    let v = page.evaluate(expr).await?;
    let s = v.value().and_then(|v| v.as_str()).unwrap_or("[]");
    let arr: Vec<Value> = serde_json::from_str(s)
        .map_err(|e| BrowserError::Decode(format!("console buffer: {e}")))?;
    Ok(arr
        .into_iter()
        .map(|e| {
            let level = e.get("level").and_then(|l| l.as_str()).unwrap_or("info");
            // Mask secrets in console text and source URL: console messages can
            // echo credentials (e.g. a logged request object containing an
            // authorization header). The final trace writer also masks, but we
            // mask here too so `browser_console` returns safe values directly.
            let text = secrets::mask_secret(e.get("text").and_then(|t| t.as_str()).unwrap_or(""));
            let url = e
                .get("url")
                .and_then(|u| u.as_str())
                .map(secrets::mask_url_secrets);
            let line = e.get("line").and_then(|l| l.as_i64());
            ConsoleEntry {
                level: ConsoleLevel::from_cdp(level),
                text,
                url,
                line,
            }
        })
        .collect())
}
