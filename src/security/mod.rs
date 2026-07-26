//! Security policy layer (allowlist/denylist) and safe defaults.
//!
//! ## Why
//! An AI agent drives the browser, so it can navigate anywhere. To bound blast
//! radius in CI/automation, callers may restrict navigation to specific hosts.
//! This module defines a pluggable policy gate. The policy is enforced by
//! [`Session::open`]/`goto`/`reload`: navigation to a host not on the allow list
//! (or on the deny list) returns [`BrowserError::NavigationBlocked`] before any
//! request is made. Configure it via the CLI `--allow-host`/`--deny-host` flags
//! or [`Session::with_policy_async`].

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
    /// True if any host restriction is configured.
    pub fn is_active(&self) -> bool {
        !self.allow_hosts.is_empty() || !self.deny_hosts.is_empty()
    }

    /// True if navigation to `url` is permitted.
    ///
    /// ## Why schemes are checked before hosts
    /// Host matching alone is not a bound. `file:///etc/passwd`, `data:...`,
    /// and `javascript:...` have no host at all, so every host pattern misses
    /// them and they sail past a deny list. A policy that says "only talk to
    /// localhost" must not leave the local filesystem wide open, so once any
    /// restriction is configured we allow only `http`/`https` (plus
    /// `about:blank`, which the session itself navigates to). With no policy
    /// configured the default stays fully permissive.
    pub fn allows(&self, url: &str) -> Result<(), PolicyDenial> {
        let parsed = Url::parse(url).ok();
        let scheme = parsed
            .as_ref()
            .map(|u| u.scheme().to_ascii_lowercase())
            .unwrap_or_default();

        if self.is_active() && !is_navigable_scheme(&scheme, url) {
            return Err(PolicyDenial::DeniedScheme(if scheme.is_empty() {
                url.to_string()
            } else {
                scheme
            }));
        }

        let host = parsed
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

/// Schemes a restricted session may navigate to.
fn is_navigable_scheme(scheme: &str, url: &str) -> bool {
    match scheme {
        "http" | "https" => true,
        // The session opens about:blank itself (page creation, `browser.open`
        // with no url). Other about: targets are not navigation destinations.
        "about" => url.eq_ignore_ascii_case("about:blank"),
        _ => false,
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
    /// The URL scheme is not navigable while a host policy is active.
    #[error("scheme '{0}' is not allowed while a host policy is active (only http/https)")]
    DeniedScheme(String),
}

/// Match a pattern (supports exact and suffix `*.example.com`).
///
/// Matching is ASCII-case-insensitive on both branches. An earlier version
/// compared the wildcard branch with `==`/`ends_with`, so `--allow-host
/// '*.Example.com'` matched nothing at all while the exact branch was already
/// case-insensitive — the same pattern behaving two different ways.
fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        pattern == host
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

    /// Regression: `file:`/`data:`/`javascript:` URLs have no host, so every
    /// host pattern missed them and they passed a deny list untouched.
    #[test]
    fn hostless_schemes_blocked_when_policy_active() {
        let deny_only = Policy {
            allow_hosts: vec![],
            deny_hosts: vec!["evil.com".into()],
        };
        assert!(deny_only.allows("file:///etc/passwd").is_err());
        assert!(deny_only.allows("data:text/html,<h1>x</h1>").is_err());
        assert!(deny_only.allows("javascript:alert(1)").is_err());
        assert!(deny_only.allows("chrome://settings").is_err());
        // Normal navigation is unaffected.
        assert!(deny_only.allows("https://example.com").is_ok());
        // about:blank is how the session opens a fresh page.
        assert!(deny_only.allows("about:blank").is_ok());

        let allow_only = Policy {
            allow_hosts: vec!["localhost".into()],
            deny_hosts: vec![],
        };
        assert!(allow_only.allows("file:///etc/passwd").is_err());
    }

    /// With no policy configured the default stays permissive.
    #[test]
    fn empty_policy_allows_everything() {
        let p = Policy::default();
        assert!(p.allows("file:///etc/passwd").is_ok());
        assert!(p.allows("https://anything.example").is_ok());
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
