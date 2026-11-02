//! Turns the request path into the upstream URL.
//!
//! The scheme is mandatory, so only these forms are accepted (the leading `/`
//! separates the proxy path from the target):
//!
//! ```text
//! /https://www.google.com/search?q=rust
//! /http://127.0.0.1:8080/api/items
//! /https%3A%2F%2Fwww.google.com   -> https://www.google.com
//! ```
//!
//! `/www.google.com` is rejected on purpose: guessing the scheme would silently
//! choose plaintext or TLS for the target.
//!
//! A single slash after the scheme is also accepted, because reverse proxies
//! frequently collapse `//` in the path before the request gets here.

use std::borrow::Cow;
use std::fmt;

use percent_encoding::percent_decode_str;
use reqwest::Url;

#[derive(Debug)]
pub enum TargetError {
    Missing,
    NotUtf8,
    MissingScheme,
    Invalid(String),
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "no target url found, expected a path such as /https://example.com"
            ),
            Self::NotUtf8 => write!(f, "the target url is not valid utf-8"),
            Self::MissingScheme => write!(
                f,
                "the target url must start with http:// or https://, e.g. /https://example.com"
            ),
            Self::Invalid(reason) => write!(f, "invalid target url: {reason}"),
        }
    }
}

impl std::error::Error for TargetError {}

/// Extracts the upstream URL from the request path (plus optional query string).
///
/// The query string always belongs to the target, so `/https://host/path?a=1`
/// proxies `https://host/path?a=1`.
pub fn parse(path_and_query: &str) -> Result<Url, TargetError> {
    let (raw_path, raw_query) = match path_and_query.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path_and_query, None),
    };

    let decoded = percent_decode_str(raw_path.trim_start_matches('/'))
        .decode_utf8()
        .map_err(|_| TargetError::NotUtf8)?;
    let decoded = decoded.trim();

    if decoded.is_empty() {
        return Err(TargetError::Missing);
    }

    let mut url = normalize(decoded)?;

    // `Url::parse` cannot see the query when the target came from the request
    // path, so attach it here.
    if url.query().is_none() {
        if let Some(query) = raw_query.filter(|query| !query.is_empty()) {
            url.set_query(Some(query));
        }
    }

    Ok(url)
}

fn normalize(raw: &str) -> Result<Url, TargetError> {
    let raw = repair_merged_slashes(raw);

    if !has_http_scheme(&raw) {
        return Err(TargetError::MissingScheme);
    }

    let url = Url::parse(&raw).map_err(|err| TargetError::Invalid(err.to_string()))?;

    if url.host_str().is_none_or(str::is_empty) {
        return Err(TargetError::Invalid("missing host".to_owned()));
    }

    Ok(url)
}

/// Puts back the second slash of the scheme when an intermediary removed it.
///
/// nginx (`merge_slashes on`, the default) and several CDNs rewrite
/// `/https://host/path` to `/https:/host/path` before the request reaches the
/// proxy. Rejecting those requests would make the proxy unusable behind them,
/// so the slash is restored. Guessing a scheme is still not allowed: the scheme
/// text has to be present in the path already.
fn repair_merged_slashes(raw: &str) -> Cow<'_, str> {
    for scheme in ["https:", "http:"] {
        let Some(rest) = strip_prefix_ignore_ascii_case(raw, scheme) else {
            continue;
        };

        // A well-formed `//` and anything else that is not exactly one slash
        // is left untouched, so the error reporting stays accurate.
        return match rest.strip_prefix('/') {
            Some(host) if !host.starts_with('/') => Cow::Owned(format!("{scheme}//{host}")),
            _ => Cow::Borrowed(raw),
        };
    }

    Cow::Borrowed(raw)
}

fn has_http_scheme(raw: &str) -> bool {
    let raw = raw.as_bytes();
    starts_with_ignore_ascii_case(raw, b"http://")
        || starts_with_ignore_ascii_case(raw, b"https://")
}

fn strip_prefix_ignore_ascii_case<'a>(haystack: &'a str, prefix: &str) -> Option<&'a str> {
    starts_with_ignore_ascii_case(haystack.as_bytes(), prefix.as_bytes())
        .then(|| &haystack[prefix.len()..])
}

fn starts_with_ignore_ascii_case(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_path(path: &str) -> Result<Url, TargetError> {
        parse(path)
    }

    #[test]
    fn parses_absolute_https_url() {
        assert_eq!(
            parse_path("/https://www.google.com/search?q=rust").unwrap(),
            Url::parse("https://www.google.com/search?q=rust").unwrap()
        );
    }

    #[test]
    fn parses_absolute_http_url() {
        assert_eq!(
            parse_path("/http://127.0.0.1:8080/api/items").unwrap(),
            Url::parse("http://127.0.0.1:8080/api/items").unwrap()
        );
    }

    #[test]
    fn accepts_uppercase_scheme() {
        assert_eq!(
            parse_path("/HTTPS://Example.COM/Path").unwrap(),
            Url::parse("https://example.com/Path").unwrap()
        );
    }

    #[test]
    fn parses_percent_encoded_url() {
        assert_eq!(
            parse_path("/https%3A%2F%2Fexample.com%2Fa%3Fb%3D1").unwrap(),
            Url::parse("https://example.com/a?b=1").unwrap()
        );
    }

    #[test]
    fn keeps_absolute_target_query() {
        assert_eq!(
            parse_path("/https://example.com/a?b=1&c=2").unwrap(),
            Url::parse("https://example.com/a?b=1&c=2").unwrap()
        );
    }

    #[test]
    fn rejects_missing_target() {
        assert!(matches!(parse_path("/"), Err(TargetError::Missing)));
        assert!(matches!(parse_path(""), Err(TargetError::Missing)));
    }

    #[test]
    fn repairs_schemes_merged_by_a_proxy() {
        assert_eq!(
            parse_path("/https:/www.google.com/search?q=rust").unwrap(),
            Url::parse("https://www.google.com/search?q=rust").unwrap()
        );
        assert_eq!(
            parse_path("/http:/127.0.0.1:8080/api/items").unwrap(),
            Url::parse("http://127.0.0.1:8080/api/items").unwrap()
        );
        assert_eq!(
            parse_path("/HTTPS:/Example.COM/Path").unwrap(),
            Url::parse("https://example.com/Path").unwrap()
        );
    }

    #[test]
    fn leaves_intact_and_broken_schemes_alone() {
        assert_eq!(
            parse_path("/https://example.com/a").unwrap(),
            Url::parse("https://example.com/a").unwrap()
        );

        for path in ["/https:", "/https:/", "/https:example.com"] {
            assert!(
                parse_path(path).is_err(),
                "{path} should not become a valid target"
            );
        }
    }

    #[test]
    fn rejects_urls_without_explicit_scheme() {
        for path in [
            "/example.com",
            "/www.google.com/a/b",
            "//example.com",
            "/example.com:8080/x",
        ] {
            assert!(
                matches!(parse_path(path), Err(TargetError::MissingScheme)),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_http_schemes() {
        for path in [
            "/ftp://example.com/file.txt",
            "/file:///etc/passwd",
            "/ws://example.com",
        ] {
            assert!(
                matches!(parse_path(path), Err(TargetError::MissingScheme)),
                "{path} should be rejected"
            );
        }
    }
}
