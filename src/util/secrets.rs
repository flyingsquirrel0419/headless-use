//! Secret masking for logs, traces, and reports.
//!
//! ## Why
//! Traces capture everything the agent does, including typed passwords and
//! network headers. We redact credential-shaped text so a trace can usually be
//! shared without leaking secrets.
//!
//! ## What this is (and is not)
//! This is a *heuristic textual* scrubber, not a guarantee. [`mask_secret`]
//! scans a string for secret-shaped substrings anywhere in it (auth schemes,
//! JWTs, AWS keys, well-known vendor prefixes, long opaque tokens, and values
//! under a sensitive-looking key name) and replaces them. It is deliberately
//! biased toward over-masking, but it *will* miss secrets it cannot recognise,
//! for example:
//!
//! - short, low-entropy or natural-language secrets that are not attached to a
//!   sensitive key name (`password is hunter2` masks nothing: `hunter2` is
//!   indistinguishable from an ordinary word),
//! - secrets that have been encoded, split, or reassembled by page code,
//! - vendor token formats not in the prefix table below,
//! - anything under a key name whose words are not in [`is_secret_key`].
//!
//! Conversely it can redact non-secrets (long hashes, UUIDs, opaque ids). Where
//! the two failure modes conflict we prefer masking; every deliberate
//! *non*-masking decision is pinned by a test in this file so the tradeoff is
//! explicit.
//!
//! ## Hard limitation: pixels cannot be masked
//! Screenshots and the MJPEG viewer stream are images. Nothing in this module
//! (or anywhere else) inspects them, so a token that is visible on screen —
//! rendered in the page, in a devtools panel, in an autofilled field — is
//! captured verbatim into `report.html` and into any saved frame. Treat
//! screenshots and viewer recordings of an authenticated session as secret
//! material regardless of what the text masking does.

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

/// Replacement for an auth-scheme credential (`Bearer …`) or a JWT.
const MARK_TOKEN: &str = "[REDACTED:token]";
/// Replacement for a standalone opaque/high-entropy secret.
const MARK_SECRET: &str = "[REDACTED:secret]";
/// Replacement for the value of a `key=value` / `key: value` pair.
const MARK_VALUE: &str = "[REDACTED]";

/// Auth schemes whose following credential is always a secret.
const AUTH_SCHEMES: &[&str] = &["bearer", "token", "basic"];

/// Well-known token prefixes that identify a secret regardless of entropy.
/// (`AKIA…` access key ids are handled separately by [`is_aws_key_id`].)
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxa-",
    "xoxp-",
    "xoxr-",
    "xoxs-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "pk_live_",
    "sk-ant-",
    "sk-proj-",
    "aiza",
];

/// Prefixes of AWS access key ids (`AKIA…`, `ASIA…`, …).
const AWS_ID_PREFIXES: &[&str] = &[
    "AKIA", "ASIA", "AIDA", "AROA", "AGPA", "ANPA", "ANVA", "AIPA", "ABIA", "ACCA",
];

/// Mask secret-shaped substrings anywhere in `s`.
///
/// The scan is a single left-to-right pass: the input is split into
/// whitespace-delimited words, each word into delimiter/"piece" runs, and each
/// piece is classified once. Nothing is re-scanned, so cost is linear in the
/// input length (this runs on every console line and every network URL).
pub fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }

    // Word spans (maximal runs of non-whitespace), collected in one pass.
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(w) = start.take() {
                words.push((w, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(w) = start {
        words.push((w, s.len()));
    }

    let mut out = String::with_capacity(s.len() + 16);
    let mut pos = 0usize; // bytes of `s` already written to `out`
    let mut i = 0usize; // current word
    let mut off = 0usize; // bytes of the current word already written
    while i < words.len() {
        let (ws, we) = words[i];
        if off == 0 {
            // Whitespace before this word. (`pos` is re-set on every path below.)
            out.push_str(&s[pos..ws]);
        }
        let word = &s[ws + off..we];
        if word.is_empty() {
            pos = we;
            off = 0;
            i += 1;
            continue;
        }
        let next = words.get(i + 1).map(|&(a, b)| &s[a..b]);

        // 1. `Bearer <tok>` / `Token <tok>` / `Basic <b64>` spanning two words.
        // Only the credential itself is consumed from the next word; anything
        // trailing it (`abc"},"id":7}`) keeps being scanned.
        if let Some((repl, consumed)) = try_scheme(word, next) {
            out.push_str(&repl);
            pos = words[i + 1].0 + consumed;
            off = consumed;
            i += 1;
            continue;
        }

        // 2. Everything inside this word.
        let (repl, expects_value) = mask_word(word);
        out.push_str(&repl);

        // 3. `key:` / `key=` at the end of a word: the value is the next word
        //    (unless an auth scheme starts there, which rule 1 handles better).
        if expects_value {
            if let Some(nw) = next {
                let scheme_follows = try_scheme(nw, words.get(i + 2).map(|&(a, b)| &s[a..b]));
                if scheme_follows.is_none() {
                    if let Some((lead, core_end)) = value_span(nw) {
                        out.push_str(&s[we..words[i + 1].0]);
                        out.push_str(&nw[..lead]);
                        out.push_str(MARK_VALUE);
                        pos = words[i + 1].0 + core_end;
                        off = core_end;
                        i += 1;
                        continue;
                    }
                }
            }
        }
        pos = we;
        off = 0;
        i += 1;
    }
    out.push_str(&s[pos..]);
    out
}

/// Leading punctuation and end offset of the first piece of `w`, for a word
/// that is the value of a sensitive key. `None` when there is nothing maskable
/// (empty, or already a redaction marker).
fn value_span(w: &str) -> Option<(usize, usize)> {
    let lead = w
        .find(|c: char| !matches!(c, '"' | '\'' | '(' | '[' | '{' | '<'))
        .unwrap_or(w.len());
    let b = w.as_bytes();
    let mut end = lead;
    while end < b.len() && is_piece_char(b[end]) {
        end += 1;
    }
    if end == lead || is_marker(&w[lead..end]) {
        return None;
    }
    Some((lead, end))
}

/// True if `s` is already one of our redaction markers (keeps masking
/// idempotent — traces are masked at more than one layer).
fn is_marker(s: &str) -> bool {
    s == "REDACTED" || s.starts_with("REDACTED:")
}

/// Bytes that may appear inside a candidate secret "piece". Non-ASCII bytes
/// count so that a non-ASCII value under a sensitive key (`password=秘密`) is
/// still masked; every multi-byte char is either wholly in or wholly out of a
/// piece, so byte-index slicing stays UTF-8 safe.
fn is_piece_char(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, b'_' | b'-' | b'.' | b'+' | b'/' | b'=' | b'~')
        || c >= 0x80
}

/// `Bearer <token>` and friends. When `word` ends with an auth scheme keyword
/// and `next` starts with a credential, returns the replacement text for
/// `word` (its non-keyword prefix plus the marker) and how many bytes of
/// `next` the credential occupies.
fn try_scheme(word: &str, next: Option<&str>) -> Option<(String, usize)> {
    let next = next?;
    let (lead, mut core_end) = value_span(next)?;
    // Do not swallow a sentence-ending period.
    while core_end > lead && next.as_bytes()[core_end - 1] == b'.' {
        core_end -= 1;
    }
    let next_core = &next[lead..core_end];
    if !is_tokenish(next_core) {
        return None;
    }
    let wb = word.as_bytes();
    for kw in AUTH_SCHEMES {
        if word.len() < kw.len() {
            continue;
        }
        let at = word.len() - kw.len();
        if !wb[at..].eq_ignore_ascii_case(kw.as_bytes()) {
            continue;
        }
        // `at` is a char boundary because the suffix is pure ASCII.
        // The keyword must start the word or follow a structural separator, so
        // `X-Auth-Token` / `subtoken` do not count as a scheme introduction.
        let ok = at == 0
            || matches!(
                wb[at - 1],
                b':' | b'"' | b'\'' | b'=' | b',' | b'(' | b'[' | b'{' | b'>' | b'|'
            );
        if !ok {
            continue;
        }
        return Some((
            format!("{}{}{}", &word[..at], &next[..lead], MARK_TOKEN),
            core_end,
        ));
    }
    None
}

/// Mask inside a single whitespace-delimited word.
///
/// Returns the masked word and whether the word ends with a sensitive key plus
/// its separator (so the caller should treat the *next* word as its value).
fn mask_word(word: &str) -> (String, bool) {
    // Data URIs are payloads, not credentials, and can be enormous. Copy through.
    // (Byte comparison: `word` may hold multi-byte UTF-8, so slicing by index
    // is only safe on runs we already know are ASCII.)
    if word.len() >= 5 && word.as_bytes()[..5].eq_ignore_ascii_case(b"data:") {
        return (word.to_string(), false);
    }
    // Inside a URL, path segments are not treated as opaque secrets (too many
    // false positives); key=value pairs, JWTs and vendor tokens still are.
    let url_mode = word.contains("://") || word.starts_with('/');

    let b = word.as_bytes();
    let mut out = String::with_capacity(word.len());
    let mut i = 0usize;
    let mut pending_key = false;
    let mut sep_after_key = false;
    while i < b.len() {
        // Delimiter run.
        let d0 = i;
        while i < b.len() && !is_piece_char(b[i]) {
            i += 1;
        }
        if i > d0 {
            let d = &word[d0..i];
            out.push_str(d);
            if pending_key && !sep_after_key {
                let stripped: String = d.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
                if stripped.is_empty() || stripped == ":" {
                    sep_after_key = true;
                } else {
                    pending_key = false;
                }
            }
        }
        if i >= b.len() {
            break;
        }
        // Piece run.
        let p0 = i;
        while i < b.len() && is_piece_char(b[i]) {
            i += 1;
        }
        let piece = &word[p0..i];
        if pending_key && sep_after_key {
            out.push_str(&mask_piece_value(piece));
            pending_key = false;
            sep_after_key = false;
            // `secret:https://host/path` — the whole URL is the value, so drop
            // the rest of the word into the same marker instead of emitting
            // `[REDACTED]://host/path`.
            if !is_marker(piece) && word[i..].starts_with("://") {
                break;
            }
            continue;
        }
        let (repl, expects) = analyze_piece(piece, url_mode);
        out.push_str(&repl);
        pending_key = expects;
        sep_after_key = expects && piece.ends_with('=');
    }
    (out, pending_key && sep_after_key)
}

/// A piece that sits directly after `sensitive_key:` — masked whatever it is.
fn mask_piece_value(piece: &str) -> String {
    if is_marker(piece) {
        return piece.to_string();
    }
    MARK_VALUE.to_string()
}

/// Classify one piece. Returns the (possibly masked) text and whether the piece
/// is a bare sensitive key name still waiting for its value.
fn analyze_piece(piece: &str, url_mode: bool) -> (String, bool) {
    // Trailing sentence punctuation that made it into the piece charset.
    let core = piece.trim_end_matches('.');
    let tail = &piece[core.len()..];
    if core.is_empty() || is_marker(core) {
        return (piece.to_string(), false);
    }

    // `key=value` (possibly nested, e.g. `foo=bar=<jwt>`); bounded loop keeps
    // this linear and non-recursive.
    let mut prefix_len = 0usize;
    let mut rest = core;
    for _ in 0..8 {
        let Some(eq) = rest.find('=') else { break };
        let (k, v) = (&rest[..eq], &rest[eq + 1..]);
        // Not a `k=v` pair but base64 padding (`…9PQ==`) or a blob.
        if !is_key_shaped(k) || looks_like_opaque(k) {
            break;
        }
        if is_secret_key(k) {
            if v.is_empty() || v.bytes().all(|c| c == b'=') {
                // `password=` — value is in the next piece/word.
                return (piece.to_string(), true);
            }
            return (
                format!("{}{k}={MARK_VALUE}{tail}", &core[..prefix_len]),
                false,
            );
        }
        prefix_len += eq + 1;
        rest = v;
    }
    let (head, body) = core.split_at(prefix_len);

    let marked = if is_jwt(body) {
        Some(MARK_TOKEN)
    } else if is_aws_key_id(body)
        || has_token_prefix(body)
        // Path segments of a URL are not treated as opaque secrets.
        || (!url_mode && looks_like_opaque(body))
    {
        Some(MARK_SECRET)
    } else {
        None
    };
    if let Some(m) = marked {
        return (format!("{head}{m}{tail}"), false);
    }
    if prefix_len == 0 && is_key_shaped(core) && is_secret_key(core) {
        return (piece.to_string(), true);
    }
    (piece.to_string(), false)
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
        "passphrase",
        "pwd",
        "auth",
        "credential",
        "credentials",
        "apikey",
        "authorization",
        "session",
        "sessionid",
        "cookie",
        "cookies",
        "signature",
    ];
    key_segments(key)
        .iter()
        .any(|seg| SECRET_WORDS.contains(&seg.as_str()))
}

/// Plausible key name: short, no exotic characters, contains a letter.
fn is_key_shaped(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'[' | b']'))
        && k.bytes().any(|c| c.is_ascii_alphabetic())
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

/// Credential-shaped word following an auth scheme keyword.
fn is_tokenish(s: &str) -> bool {
    if s.len() < 6 {
        return false;
    }
    if !s.bytes().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+' | b'/' | b'=' | b'~')
    }) {
        return false;
    }
    let has_digit = s.bytes().any(|c| c.is_ascii_digit());
    let has_upper = s.bytes().any(|c| c.is_ascii_uppercase());
    let has_lower = s.bytes().any(|c| c.is_ascii_lowercase());
    // An all-lowercase alphabetic word is almost always prose ("Token expired"),
    // so only accept it when it is implausibly long for a word.
    has_digit || (has_upper && has_lower) || s.len() >= 16
}

/// JWT: three base64url segments, the header starting with `eyJ` (`{"`).
fn is_jwt(s: &str) -> bool {
    if s.len() < 20 || !s.starts_with("eyJ") {
        return false;
    }
    let mut parts = s.split('.');
    let (Some(h), Some(p), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if h.len() < 8 || p.len() < 2 {
        return false;
    }
    [h, p, sig]
        .iter()
        .all(|seg| seg.bytes().all(is_base64url_byte))
}

fn is_base64url_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'=')
}

/// AWS access key id: `AKIA` (or sibling prefix) + 16 uppercase alphanumerics.
fn is_aws_key_id(s: &str) -> bool {
    s.len() == 20
        && s.bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && AWS_ID_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// Vendor token with a recognisable prefix (GitHub, Slack, Stripe, Google, ...).
fn has_token_prefix(s: &str) -> bool {
    if s.len() < 16 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    TOKEN_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Long opaque high-entropy token: base64 / base64url / hex, no separators that
/// make it read like a hyphenated name or a filesystem path.
fn looks_like_opaque(s: &str) -> bool {
    let body = s.trim_end_matches('=');
    if body.len() < 32 {
        return false;
    }
    if !body
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+' | b'/'))
    {
        return false;
    }
    let has_digit = body.bytes().any(|c| c.is_ascii_digit());
    let has_alpha = body.bytes().any(|c| c.is_ascii_alphabetic());
    let has_upper = body.bytes().any(|c| c.is_ascii_uppercase());
    let has_lower = body.bytes().any(|c| c.is_ascii_lowercase());
    if !((has_digit && has_alpha) || (has_upper && has_lower)) {
        return false;
    }
    // `some-long-css-class-name-2`, `/a/deep/path/of/words`: separated into
    // segments that are each a plain word or a plain number. Random tokens are
    // not shaped like that.
    let separated = body.bytes().any(|c| matches!(c, b'-' | b'_' | b'/'));
    if separated {
        let all_wordlike = body
            .split(['-', '_', '/'])
            .filter(|seg| !seg.is_empty())
            .all(|seg| {
                seg.bytes().all(|c| c.is_ascii_alphabetic())
                    || seg.bytes().all(|c| c.is_ascii_digit())
            });
        if all_wordlike {
            return false;
        }
    }
    true
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
mod tests {
    use super::*;
    use serde_json::json;

    const JWT: &str =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const AWS_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const AWS_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

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
        assert_eq!(m["url"], "http://x");
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

    // ---- defect 1: JWTs ----

    #[test]
    fn masks_jwt_standalone_and_embedded() {
        assert_eq!(mask_secret(JWT), "[REDACTED:token]");
        let line = format!("failed to verify {JWT} (exp)");
        let masked = mask_secret(&line);
        assert!(!masked.contains("eyJ"), "{masked}");
        assert_eq!(masked, "failed to verify [REDACTED:token] (exp)");
        // In a fragment, where mask_url_secrets does not look.
        let cb = format!("https://app.example.com/cb#id_token={JWT}&state=1");
        let masked = mask_secret(&cb);
        assert!(!masked.contains("eyJ"), "{masked}");
        assert!(masked.starts_with("https://app.example.com/cb#id_token=[REDACTED]"));
    }

    // ---- defect 2: AWS keys ----

    #[test]
    fn masks_aws_access_key_id_and_secret() {
        assert_eq!(mask_secret(AWS_ID), "[REDACTED:secret]");
        assert_eq!(mask_secret(AWS_SECRET), "[REDACTED:secret]");
        let line = format!("aws creds {AWS_ID} / {AWS_SECRET} loaded");
        let masked = mask_secret(&line);
        assert!(!masked.contains("AKIA"), "{masked}");
        assert!(!masked.contains("wJalr"), "{masked}");
    }

    // ---- defect 3: Basic auth ----

    #[test]
    fn masks_basic_auth() {
        assert_eq!(mask_secret("Basic dXNlcjpwYXNz"), "[REDACTED:token]");
        assert_eq!(
            mask_secret("Authorization: Basic dXNlcjpwYXNz"),
            "Authorization: [REDACTED:token]"
        );
        assert_eq!(mask_secret("Token abc123def"), "[REDACTED:token]");
    }

    // ---- defect 4: secrets embedded in a longer string ----

    #[test]
    fn masks_secret_embedded_in_console_line() {
        let line = format!("GET /api failed, Authorization: Bearer {JWT} rejected");
        let masked = mask_secret(&line);
        assert_eq!(
            masked,
            "GET /api failed, Authorization: [REDACTED:token] rejected"
        );

        let line = "password is hunter2xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let masked = mask_secret(line);
        assert!(!masked.contains("hunter2x"), "{masked}");

        // JSON echoed into console on one line.
        let line = r#"request {"headers":{"Authorization":"Bearer abc123xyz"},"id":7}"#;
        let masked = mask_secret(line);
        assert!(!masked.contains("abc123xyz"), "{masked}");
        assert!(masked.contains(r#""[REDACTED:token]""#), "{masked}");
    }

    // ---- defect 5: short value under a sensitive key name ----

    #[test]
    fn masks_short_value_under_sensitive_key() {
        assert_eq!(mask_secret("password=hunter2"), "password=[REDACTED]");
        assert_eq!(mask_secret("password: hunter2"), "password: [REDACTED]");
        assert_eq!(mask_secret("session_id: 42"), "session_id: [REDACTED]");
        assert_eq!(
            mask_secret(r#"{"apiKey":"abc"}"#),
            r#"{"apiKey":"[REDACTED]"}"#
        );
        assert_eq!(
            mask_secret("login failed for user bob, passwd=abc, retrying"),
            "login failed for user bob, passwd=[REDACTED], retrying"
        );
        // A URL-valued secret is masked whole, not turned into `[REDACTED]://…`.
        assert_eq!(
            mask_secret("cookie:https://evil.example.com/cb?x=1 sent"),
            "cookie:[REDACTED] sent"
        );
        // Hyphen/underscore variants of the key name.
        for k in [
            "api-key",
            "api_key",
            "private-key",
            "access_key",
            "Credential",
            "cookie",
        ] {
            let masked = mask_secret(&format!("{k}=zz"));
            assert_eq!(masked, format!("{k}=[REDACTED]"), "key {k}");
        }
    }

    #[test]
    fn masks_long_opaque_tokens_anywhere() {
        let tok = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        assert_eq!(mask_secret(tok), "[REDACTED:secret]");
        assert_eq!(
            mask_secret(&format!("got secret {tok} back")),
            "got secret [REDACTED:secret] back"
        );
        // `token <credential>` is an auth-scheme introduction, so the keyword
        // is folded into the marker.
        assert_eq!(
            mask_secret(&format!("got token {tok} back")),
            "got [REDACTED:token] back"
        );
        // base64 with +/= and a 32-char hex digest.
        assert_eq!(
            mask_secret("blob dGhpcytpcy9hK2Jhc2U2NC9zdHJpbmc9PQ== end"),
            "blob [REDACTED:secret] end"
        );
        assert_eq!(
            mask_secret("etag d41d8cd98f00b204e9800998ecf8427e."),
            "etag [REDACTED:secret]."
        );
        // Vendor prefixes shorter than the entropy threshold.
        assert_eq!(mask_secret("ghp_16CharsMinimum01"), "[REDACTED:secret]");
        assert_eq!(mask_secret("xoxb-123456789012-ab"), "[REDACTED:secret]");
    }

    #[test]
    fn masking_is_idempotent() {
        for input in [
            "Bearer abc123",
            "password: hunter2",
            &format!("Authorization: Bearer {JWT}"),
            AWS_SECRET,
            "https://x/y?access_token=abc",
        ] {
            let once = mask_secret(input);
            assert_eq!(mask_secret(&once), once, "not idempotent: {input}");
        }
    }

    // ---- deliberate NON-masking decisions (false-positive pins) ----
    //
    // Each of these is a shape we could mask but choose not to, because the
    // cost in unreadable traces outweighs the (low) chance it is a credential.

    /// Byte-index scanning must never split a UTF-8 char, and the scan must
    /// stay linear: these inputs are the pathological shapes (many word starts,
    /// one huge word) that a naive re-scanning matcher turns quadratic.
    #[test]
    fn handles_unicode_and_large_inputs() {
        let s = "ключ пароль: секрет — Bearer abc123 — 日本語 password=秘密";
        let masked = mask_secret(s);
        assert!(masked.contains("[REDACTED:token]"), "{masked}");
        assert!(masked.contains("password=[REDACTED]"), "{masked}");

        let hyphens = "a-".repeat(200_000);
        assert_eq!(mask_secret(&hyphens).len(), hyphens.len());
        let words = "word ".repeat(200_000);
        assert_eq!(mask_secret(&words), words);
        let blob = "x".repeat(1_000_000);
        assert_eq!(mask_secret(&blob), blob);
    }

    /// Accepted over-masking: standalone long digests and UUID-shaped ids are
    /// indistinguishable from opaque credentials, so they are redacted. Ids
    /// that appear inside a URL path are not (see the URL pin above).
    #[test]
    fn accepted_over_masking_of_digests_and_uuids() {
        assert_eq!(
            mask_secret("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
            "sha256:[REDACTED:secret]"
        );
        assert_eq!(
            mask_secret("run 550e8400-e29b-41d4-a716-446655440000 started"),
            "run [REDACTED:secret] started"
        );
        // Plain long numbers (timestamps, ids) keep their meaning.
        assert_eq!(
            mask_secret("the quick brown fox 0123456789012345678901234567890123 jumps"),
            "the quick brown fox 0123456789012345678901234567890123 jumps"
        );
    }

    /// Real console noise that must survive untouched.
    #[test]
    fn does_not_mask_typical_console_lines() {
        for s in [
            "Uncaught TypeError: Cannot read properties of undefined (reading 'map')",
            "Failed to load resource: the server responded with a status of 401 (Unauthorized)",
            "[HMR] Waiting for update signal from WDS... http://localhost:3000/sockjs-node/info?t=1699999999999",
            "Refused to load the stylesheet 'https://fonts.googleapis.com/css2?family=Inter:wght@400;700&display=swap'",
            "user@example.com logged in at 2024-01-02T03:04:05.678Z",
            "npm WARN deprecated core-js@2.6.12: core-js@<3.23.3 is no longer maintained",
            "webpack://./src/components/Button/Button.tsx?f00d",
        ] {
            assert_eq!(mask_secret(s), s, "console line masked: {s}");
        }
    }

    #[test]
    fn masks_set_cookie_header_line() {
        assert_eq!(
            mask_secret("Set-Cookie: sessionid=3f8a9c2b1d4e5f6a7b8c9d0e1f2a3b4c; HttpOnly; Secure"),
            "Set-Cookie: [REDACTED]; HttpOnly; Secure"
        );
    }

    #[test]
    fn does_not_mask_ordinary_prose() {
        for s in [
            "the quick brown fox jumps over the lazy dog",
            "Token expired, please sign in again",
            "authorization failed for this bearer of bad news",
            "an internationalization antidisestablishment discussion",
        ] {
            assert_eq!(mask_secret(s), s, "prose masked: {s}");
        }
    }

    #[test]
    fn does_not_mask_css_classes_or_hex_colors() {
        for s in [
            "class=\"btn btn-primary rounded-lg shadow-md hover:bg-blue-500\"",
            "color: #ff00aa; background: #1a2b3c",
            "grid-template-columns-repeat-auto-fill-minmax",
        ] {
            assert_eq!(mask_secret(s), s, "css masked: {s}");
        }
    }

    #[test]
    fn does_not_mask_data_uris() {
        let s = "img data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg== ok";
        assert_eq!(mask_secret(s), s);
    }

    #[test]
    fn does_not_mask_url_path_segments() {
        for s in [
            "https://cdn.example.com/assets/v2/images/2024/hero-banner-large.png",
            "GET https://api.example.com/v1/organizations/12345/members/67890/settings 200",
            "loaded /static/js/vendors~main~runtime/chunk/9f2b1c/index.js",
        ] {
            assert_eq!(mask_secret(s), s, "url masked: {s}");
        }
    }

    #[test]
    fn does_not_mask_sentences_naming_a_key_without_a_separator() {
        // `password is hunter2` has no `=`/`:` and `hunter2` is word-shaped, so
        // nothing here is recognisable as a secret. Documented miss.
        assert_eq!(mask_secret("password is hunter2"), "password is hunter2");
        assert_eq!(mask_secret("the api key rotated"), "the api key rotated");
    }
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
