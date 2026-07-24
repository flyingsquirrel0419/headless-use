//! Network observation via the injected collector script.

use serde_json::Value;

use crate::browser::{BrowserError, Page};
use crate::util::secrets;

/// One observed network request/response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkEntry {
    /// HTTP method.
    pub method: String,
    /// Request URL (secrets masked).
    pub url: String,
    /// Response status code (0/None if failed before a response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Resource type (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Duration in ms (if completed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// Failure reason (if the request failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// Error type alias for the module.
pub type NetworkError = BrowserError;

/// Read buffered network entries from the injected collector.
pub async fn collect(page: &Page) -> Result<Vec<NetworkEntry>, BrowserError> {
    let expr = r#"
    (() => {
      const out = (window.__hu_network__ || []).map(e => ({
        method: e.method, url: e.url, status: e.status || null,
        resourceType: e.resourceType || null, durationMs: e.durationMs || null,
        failed: e.failed || null
      }));
      return JSON.stringify(out);
    })()
    "#;
    let v = page.evaluate(expr).await?;
    let s = v.value().and_then(|v| v.as_str()).unwrap_or("[]");
    let arr: Vec<Value> =
        serde_json::from_str(s).map_err(|e| BrowserError::Other(format!("network parse: {e}")))?;
    Ok(arr
        .into_iter()
        .map(|e| NetworkEntry {
            method: e
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("GET")
                .to_string(),
            url: secrets::mask_secret(e.get("url").and_then(|u| u.as_str()).unwrap_or("")),
            status: e.get("status").and_then(|s| s.as_u64()).map(|s| s as u16),
            resource_type: e
                .get("resourceType")
                .and_then(|r| r.as_str())
                .map(String::from),
            duration_ms: e.get("durationMs").and_then(|d| d.as_f64()),
            failed: e.get("failed").and_then(|f| f.as_str()).map(String::from),
        })
        .collect())
}
