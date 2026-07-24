//! Security policy layer (allowlist/denylist) and safe defaults.
//!
//! ## Why
//! An AI agent drives the browser, so it can navigate anywhere. To bound blast
//! radius in CI/automation, callers may restrict navigation to specific hosts.
//! This module is a pluggable policy gate; the MVP enforces host allow/deny.

use url::Url;

/// A navigation/network policy.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// If non-empty, only these hosts are allowed.
    pub allow_hosts: Vec<String>,
    /// These hosts are always denied (takes precedence over allow).
    pub deny_hosts: Vec<String>,
}

impl Policy {
    /// True if navigation to `url` is permitted.
    pub fn allows(&self, url: &str) -> Result<(), PolicyDenial> {
        let host = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();
        if !self.deny_hosts.is_empty() && self.deny_hosts.iter().any(|h| host_matches(h, &host)) {
            return Err(PolicyDenial::DeniedHost(host));
        }
        if !self.allow_hosts.is_empty() && !self.allow_hosts.iter().any(|h| host_matches(h, &host))
        {
            return Err(PolicyDenial::NotInAllowlist(host));
        }
        Ok(())
    }
}

/// Reason a URL was denied.
#[derive(Debug, thiserror::Error)]
pub enum PolicyDenial {
    /// Host is on the deny list.
    #[error("host '{0}' is on the deny list")]
    DeniedHost(String),
    /// Host is not on the allow list.
    #[error("host '{0}' is not in the allow list")]
    NotInAllowlist(String),
}

/// Match a pattern (supports exact and suffix `*.example.com`).
fn host_matches(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        pattern.eq_ignore_ascii_case(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_blocks_others() {
        let p = Policy {
            allow_hosts: vec!["localhost".into(), "127.0.0.1".into()],
            deny_hosts: vec![],
        };
        assert!(p.allows("http://localhost:3000").is_ok());
        assert!(p.allows("http://127.0.0.1:8080").is_ok());
        assert!(p.allows("http://evil.com").is_err());
    }

    #[test]
    fn denylist_takes_precedence() {
        let p = Policy {
            allow_hosts: vec!["localhost".into()],
            deny_hosts: vec!["localhost".into()],
        };
        assert!(p.allows("http://localhost:3000").is_err());
    }

    #[test]
    fn wildcard_matching() {
        let p = Policy {
            allow_hosts: vec!["*.example.com".into()],
            deny_hosts: vec![],
        };
        assert!(p.allows("http://a.example.com").is_ok());
        assert!(p.allows("http://example.com").is_ok());
        assert!(p.allows("http://other.com").is_err());
    }
}
