//! Action trace recording + report generation (replay is on the roadmap).
//!
//! ## Why
//! When an agent's browser session misbehaves, the most useful artifact is a
//! faithful record of what it did and what the page did in response. We record
//! every action as a JSONL line, and generates a self-contained `report.html`.
//! (Replay of the action sequence is on the roadmap.) The `report.html` is a
//! self-contained static file (no external
//! deps) for sharing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::util;

/// A trace recorder writing to a run directory.
pub struct Trace {
    dir: PathBuf,
    actions_file: Option<tokio::fs::File>,
    seq: AtomicU64,
    start: Instant,
}

impl Trace {
    /// Create a new trace under `.headless-use/runs/<timestamp>-<id>/`.
    pub async fn new(base: &Path) -> Result<Self, std::io::Error> {
        let ts = util::timestamp_utc();
        let id = short_id();
        let dir = base
            .join(".headless-use")
            .join("runs")
            .join(format!("{ts}-{id}"));
        tokio::fs::create_dir_all(dir.join("screenshots")).await?;
        let actions_file = Some(tokio::fs::File::create(dir.join("actions.jsonl")).await?);
        Ok(Self {
            dir,
            actions_file,
            seq: AtomicU64::new(0),
            start: Instant::now(),
        })
    }

    /// The run directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Record an action.
    ///
    /// Secrets are masked at this boundary as a final defense layer: even if a
    /// caller forgets to set `sensitive=true`, or a new action type is added
    /// that carries credentials, the trace never persists raw secrets. The
    /// masking is conservative (it may redact non-secrets) but never leaks.
    pub async fn record(&mut self, kind: &str, params: Value) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // Final defense: mask secrets in every action's params before writing.
        // This catches password fields, tokens, API keys, and authorization
        // headers regardless of whether the caller flagged sensitivity.
        let safe_params = util::secrets::mask_json(&params);
        let entry = json!({
            "sequence": seq,
            "timestampMs": self.start.elapsed().as_millis() as u64,
            "action": { "type": kind, "params": safe_params },
        });
        if let Some(f) = self.actions_file.as_mut() {
            let line = format!("{}\n", entry);
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.flush().await;
        }
        seq
    }

    /// Save a screenshot into the run dir.
    pub async fn save_screenshot(&self, seq: u64, data: &[u8]) -> Result<PathBuf, std::io::Error> {
        let path = self.dir.join("screenshots").join(format!("{seq:04}.png"));
        util::write_bytes(&path, data)?;
        Ok(path)
    }

    /// Flush and write metadata + report.
    pub async fn flush(&mut self) -> Result<(), std::io::Error> {
        if let Some(mut f) = self.actions_file.take() {
            f.flush().await?;
        }
        let meta = json!({
            "version": crate::VERSION,
            "startedAt": util::timestamp_utc(),
            "durationMs": self.start.elapsed().as_millis(),
        });
        let meta_path = self.dir.join("metadata.json");
        tokio::fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?).await?;
        self.write_report().await?;
        Ok(())
    }

    /// Write the self-contained report.html.
    async fn write_report(&self) -> Result<(), std::io::Error> {
        let html = generate_report(&self.dir).await?;
        tokio::fs::write(self.dir.join("report.html"), html).await?;
        Ok(())
    }
}

/// Read actions.jsonl from a run dir and render report.html. Screenshot actions
/// embed the saved PNG inline (base64) so the report is fully self-contained.
async fn generate_report(dir: &Path) -> Result<String, std::io::Error> {
    let actions = tokio::fs::read_to_string(dir.join("actions.jsonl"))
        .await
        .unwrap_or_default();
    let mut rows = String::new();
    for line in actions.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let seq = v.get("sequence").and_then(|s| s.as_u64()).unwrap_or(0);
        let ts = v.get("timestampMs").and_then(|t| t.as_u64()).unwrap_or(0);
        let action = v.get("action").cloned().unwrap_or(Value::Null);
        let atype = action.get("type").and_then(|t| t.as_str()).unwrap_or("?");
        let params = action.get("params").cloned().unwrap_or(Value::Null);
        let params_str = serde_json::to_string_pretty(&params).unwrap_or_default();
        // For screenshot actions, embed the saved PNG inline as base64 so the
        // report is a single self-contained file with no external image deps.
        let img_html = if atype == "screenshot" {
            let shot_path = dir.join("screenshots").join(format!("{seq:04}.png"));
            if let Ok(data) = tokio::fs::read(&shot_path).await {
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
                format!(
                    r#"<img src="data:image/png;base64,{b64}" style="max-width:480px;border:1px solid #ccc;margin-top:4px"/>"#
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        rows.push_str(&format!(
            r#"<tr><td>{seq}</td><td>{ts}</td><td><code>{atype}</code></td><td><pre>{}</pre>{img_html}</td></tr>"#,
            html_escape(&params_str)
        ));
    }
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>headless-use trace report</title>
<style>
body {{ font-family: system-ui, sans-serif; margin: 2rem; color: #222; }}
h1 {{ font-size: 1.4rem; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ border: 1px solid #ddd; padding: 6px 10px; text-align: left; vertical-align: top; }}
th {{ background: #f4f4f4; }}
pre {{ white-space: pre-wrap; margin: 0; max-width: 60ch; }}
code {{ background: #eef; padding: 1px 4px; border-radius: 3px; }}
</style></head><body>
<h1>headless-use trace report</h1>
<table>
<thead><tr><th>#</th><th>ms</th><th>action</th><th>params</th></tr></thead>
<tbody>{rows}</tbody>
</table>
</body></html>"#
    );
    Ok(html)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn short_id() -> String {
    let bytes: [u8; 4] = std::array::from_fn(|_| fastrand::u8(0..=255));
    hex::encode(bytes)
}
