//! Secret masking for logs, traces, and reports.
//!
//! ## Why
//! Traces capture everything the agent does, including typed passwords and
//! network headers. By default we redact known-sensitive patterns so a trace
//! can be shared without leaking credentials. Masking is conservative: it may
//! redact non-secrets (false positives) but must never leak a real secret
//! (false negatives).

use serde_json::Value;

/// Header names whose values are always redacted.
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "proxy-authorization",
    "x-auth-token",
    "x-csrf-token",
];

/// Mask a string that may contain a secret. Replaces the whole value.
pub fn mask_secret(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    // bearer tokens / api keys
    if lower.starts_with("bearer ") || lower.starts_with("token ") {
        return "[REDACTED:token]".into();
    }
    // .env style KEY=VALUE for known secret-ish keys
    if let Some(eq) = s.find('=') {
        let key = &s[..eq];
        if is_secret_key(key) {
            return format!("{}=[REDACTED]", key);
        }
    }
    // long base64-ish strings resembling keys (>=32 chars, high entropy-ish)
    if s.len() >= 40 && looks_like_token(s) {
        return "[REDACTED:secret]".into();
    }
    s.to_string()
}

/// True if a key name looks like it holds a secret.
///
/// Matching is on word segments, not raw substrings. A plain `contains` check
/// flagged `author` (contains "auth"), `monkey` and `keyboard` (contain "key"),
/// which is noise in traces and URLs. Splitting on the usual key separators
/// (`_ - . space` and camelCase boundaries) keeps `api_key`, `X-Auth-Token`
/// and `accessToken` matching while leaving ordinary words alone.
pub fn is_secret_key(key: &str) -> bool {
    const SECRET_WORDS: &[&str] = &[
        "key",
        "keys",
        "secret",
        "secrets",
        "token",
        "password",
        "passwd",
        "pwd",
        "auth",
        "credential",
        "credentials",
        "apikey",
        "authorization",
        "session",
        "signature",
    ];
    key_segments(key)
        .iter()
        .any(|seg| SECRET_WORDS.contains(&seg.as_str()))
}

/// Split a key name into lowercase word segments on separators and camelCase
/// boundaries: `X-Auth-Token` -> ["x", "auth", "token"], `accessToken` ->
/// ["access", "token"].
fn key_segments(key: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' || c == '[' || c == ']' {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Mask a JSON value recursively (redact sensitive header values, etc.).
pub fn mask_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if is_secret_key(k) || SENSITIVE_HEADERS.contains(&k.to_ascii_lowercase().as_str())
                {
                    out.insert(k.clone(), Value::String("[REDACTED]".into()));
                } else if is_url_key(k) {
                    // URL-valued keys (url, href, src, ...) may carry secrets
                    // in their query string (e.g. `?token=...`). mask_secret
                    // alone does not look inside the query, so scrub the params
                    // first, then run the normal string masking on the result.
                    if let Value::String(s) = val {
                        let scrubbed = mask_url_secrets(s);
                        out.insert(k.clone(), Value::String(mask_secret(&scrubbed)));
                    } else {
                        out.insert(k.clone(), mask_json(val));
                    }
                } else {
                    out.insert(k.clone(), mask_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(mask_json).collect()),
        Value::String(s) => Value::String(mask_secret(s)),
        other => other.clone(),
    }
}

/// Keys whose string values should be treated as URLs and have their query
/// parameters scrubbed for secrets in addition to whole-value masking.
fn is_url_key(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "url" | "href" | "src" | "link" | "uri" | "endpoint" | "redirect_uri" | "callback_url"
    )
}

/// Heuristic: looks like a long opaque token (hex/base64) without spaces.
fn looks_like_token(s: &str) -> bool {
    !s.contains(' ')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().filter(|c| c.is_ascii_alphabetic()).count() > 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_bearer_and_env() {
        assert_eq!(mask_secret("Bearer abc123"), "[REDACTED:token]");
        assert_eq!(mask_secret("API_KEY=supersecret123"), "API_KEY=[REDACTED]");
    }

    #[test]
    fn leaves_plain_text() {
        assert_eq!(mask_secret("hello world"), "hello world");
        assert_eq!(mask_secret("short"), "short");
    }

    #[test]
    fn masks_json_keys() {
        let v = json!({
            "url": "http://x",
            "authorization": "Bearer xyz",
            "data": { "password": "hunter2", "name": "bob" }
        });
        let m = mask_json(&v);
        assert_eq!(m["authorization"], "[REDACTED]");
        assert_eq!(m["data"]["password"], "[REDACTED]");
        assert_eq!(m["data"]["name"], "bob");
    }

    #[test]
    fn is_secret_key_detects_common() {
        assert!(is_secret_key("api_key"));
        assert!(is_secret_key("apiKey"));
        assert!(is_secret_key("X-Auth-Token"));
        assert!(is_secret_key("PASSWORD"));
        assert!(is_secret_key("access_token"));
        assert!(is_secret_key("accessToken"));
        assert!(is_secret_key("authorization"));
        assert!(!is_secret_key("username"));
        assert!(!is_secret_key("url"));
    }

    /// Word-segment matching, not substring: these all contain a secret word
    /// but are not secrets.
    #[test]
    fn is_secret_key_ignores_words_that_merely_contain_a_secret_word() {
        for k in [
            "author",
            "authors",
            "monkey",
            "keyboard",
            "tokenizer",
            "passwordless_hint_text",
            "keynote",
        ] {
            assert!(!is_secret_key(k), "'{k}' should not be treated as a secret");
        }
    }
}

/// Mask secret-like query parameters in a URL string.
///
/// ## Why
/// Network entry URLs may carry tokens in the query string
/// (`?access_token=...`). We redact known-sensitive param values so they do
/// not appear in traces/reports.
pub fn mask_url_secrets(url: &str) -> String {
    let (scheme_rest, query_frag) = match url.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let Some(query) = query_frag else {
        return url.to_string();
    };
    let frag_split = query.split_once('#');
    let (q, frag) = match frag_split {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };
    let masked: Vec<String> = q
        .split('&')
        .map(|pair| {
            if let Some((k, _)) = pair.split_once('=') {
                if is_secret_key(k) {
                    return format!("{k}=[REDACTED]");
                }
            }
            pair.to_string()
        })
        .collect();
    let mut out = format!("{}?{}", scheme_rest, masked.join("&"));
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

#[cfg(test)]
mod url_tests {
    use super::*;
    #[test]
    fn masks_url_query_secrets() {
        assert_eq!(
            mask_url_secrets("https://api.example.com/data?token=secret&foo=bar"),
            "https://api.example.com/data?token=[REDACTED]&foo=bar"
        );
        assert_eq!(
            mask_url_secrets("https://x/y?access_token=abc#frag"),
            "https://x/y?access_token=[REDACTED]#frag"
        );
        assert_eq!(mask_url_secrets("https://x/y"), "https://x/y");
    }
}
