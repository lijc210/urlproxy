//! Access control: optional shared-token authentication and a target host
//! allowlist.
//!
//! Both are configured through environment variables and are disabled/inactive
//! by default (`PROXY_TOKEN` unset, `ALLOWED_HOSTS` empty => all hosts allowed).

use axum::http::HeaderMap;
use percent_encoding::percent_decode_str;

/// Header carrying the proxy token.
pub const TOKEN_HEADER: &str = "x-proxy-token";

/// Query parameter carrying the proxy token. Needed because a browser
/// navigation or a plain link click cannot set request headers.
pub const TOKEN_QUERY: &str = "proxy_token";

/// Entry in `ALLOWED_HOSTS` that allows every host.
const ALLOW_ALL: &str = "*";

#[derive(Debug, Clone, Default)]
pub struct Access {
    token: Option<String>,
    allowed_hosts: Vec<String>,
}

/// What `authorize` decided, plus what the handler still has to do.
#[derive(Debug)]
pub struct Authorized {
    /// Query string with `proxy_token` removed, ready to be sent to the target.
    pub query: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AccessError {
    Missing,
    Mismatch,
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "missing proxy token: pass it with the `X-Proxy-Token` header \
                 or the `proxy_token` query parameter"
            ),
            Self::Mismatch => write!(f, "invalid proxy token"),
        }
    }
}

impl std::error::Error for AccessError {}

impl Access {
    pub fn new(token: Option<String>, allowed_hosts: Vec<String>) -> Self {
        let token = token
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());

        let allowed_hosts = allowed_hosts
            .iter()
            .map(|entry| normalize_entry(entry))
            .filter(|entry| !entry.is_empty())
            .collect();

        Self {
            token,
            allowed_hosts,
        }
    }

    pub fn token_required(&self) -> bool {
        self.token.is_some()
    }

    pub fn allowed_hosts(&self) -> &[String] {
        &self.allowed_hosts
    }

    /// Checks the presented token, if authentication is enabled.
    ///
    /// The token is accepted from the `X-Proxy-Token` header or the
    /// `proxy_token` query parameter, in that order. The `Authorization` header
    /// is never treated as a proxy credential: it always belongs to the target
    /// and is forwarded untouched.
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Authorized, AccessError> {
        let Some(expected) = self.token.as_deref() else {
            return Ok(Authorized {
                query: query.map(str::to_owned),
            });
        };

        let (query_token, cleaned_query) = split_query_token(query);

        let presented = match header_token(headers) {
            Some(value) => Some(value.to_owned()),
            None => query_token,
        };

        match presented {
            None => Err(AccessError::Missing),
            Some(presented) if constant_time_eq(presented.as_bytes(), expected.as_bytes()) => {
                Ok(Authorized {
                    query: cleaned_query,
                })
            }
            Some(_) => Err(AccessError::Mismatch),
        }
    }

    /// Whether the target host passes the allowlist. An empty allowlist allows
    /// every host.
    pub fn host_allowed(&self, host: Option<&str>) -> bool {
        if self.allowed_hosts.is_empty() {
            return true;
        }

        let Some(host) = host else {
            return false;
        };

        let host = normalize_host(host);

        self.allowed_hosts
            .iter()
            .any(|entry| entry == ALLOW_ALL || host_matches(&host, entry))
    }
}

/// `example.com` matches both `example.com` and `*.example.com`; a leading
/// `*.` in the entry is accepted and means the same thing.
fn host_matches(host: &str, entry: &str) -> bool {
    host == entry || host.ends_with(&format!(".{entry}"))
}

fn normalize_entry(entry: &str) -> String {
    let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    entry
        .strip_prefix("*.")
        .unwrap_or(&entry)
        .trim_start_matches('.')
        .to_owned()
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

fn header_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Pulls `proxy_token` out of the query string and returns it together with the
/// query that should be forwarded to the target.
fn split_query_token(query: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(query) = query else {
        return (None, None);
    };

    let mut token = None;
    let mut kept = Vec::new();

    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode_str(key).decode_utf8_lossy();

        if key == TOKEN_QUERY {
            token = Some(percent_decode_str(value).decode_utf8_lossy().into_owned());
        } else {
            kept.push(pair);
        }
    }

    let cleaned = (!kept.is_empty()).then(|| kept.join("&"));
    (token, cleaned)
}

/// Compares secrets without short-circuiting on the first differing byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.iter()
        .zip(right)
        .fold(0u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(token: Option<&str>, hosts: &[&str]) -> Access {
        Access::new(
            token.map(str::to_owned),
            hosts.iter().map(|host| (*host).to_owned()).collect(),
        )
    }

    fn headers_with(name: &'static str, value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        headers
    }

    #[test]
    fn disabled_token_allows_everything() {
        let access = access(None, &[]);
        let authorized = access.authorize(&HeaderMap::new(), Some("a=1")).unwrap();

        assert!(!access.token_required());
        assert_eq!(authorized.query.as_deref(), Some("a=1"));
    }

    #[test]
    fn authorization_header_is_never_a_proxy_credential() {
        let access = access(Some("s3cret"), &[]);

        for value in ["Bearer s3cret", "bearer   s3cret"] {
            let headers = headers_with("authorization", value);
            assert_eq!(
                access.authorize(&headers, None).unwrap_err(),
                AccessError::Missing
            );
        }

        // The header is ignored, so a real credential elsewhere still works.
        let headers = headers_with("authorization", "Bearer s3cret");
        assert!(access
            .authorize(&headers, Some("proxy_token=s3cret"))
            .is_ok());
    }

    #[test]
    fn accepts_proxy_token_header() {
        let access = access(Some("s3cret"), &[]);
        let headers = headers_with("x-proxy-token", "s3cret");

        assert!(access.authorize(&headers, None).is_ok());
    }

    #[test]
    fn accepts_query_token_and_strips_it() {
        let access = access(Some("s3cret"), &[]);

        let authorized = access
            .authorize(&HeaderMap::new(), Some("a=1&proxy_token=s3cret&b=2"))
            .unwrap();

        assert_eq!(authorized.query.as_deref(), Some("a=1&b=2"));
    }

    #[test]
    fn strips_query_token_even_when_it_is_the_only_parameter() {
        let access = access(Some("s3cret"), &[]);

        let authorized = access
            .authorize(&HeaderMap::new(), Some("proxy_token=s3cret"))
            .unwrap();

        assert_eq!(authorized.query, None);
    }

    #[test]
    fn rejects_missing_and_wrong_tokens() {
        let access = access(Some("s3cret"), &[]);

        assert_eq!(
            access
                .authorize(&HeaderMap::new(), Some("a=1"))
                .unwrap_err(),
            AccessError::Missing
        );
        assert_eq!(
            access
                .authorize(&HeaderMap::new(), Some("proxy_token=nope"))
                .unwrap_err(),
            AccessError::Mismatch
        );
        assert_eq!(
            access
                .authorize(&headers_with("x-proxy-token", "nope"), None)
                .unwrap_err(),
            AccessError::Mismatch
        );
    }

    #[test]
    fn header_token_takes_precedence_over_query_token() {
        let access = access(Some("s3cret"), &[]);
        let headers = headers_with("x-proxy-token", "nope");

        // The first source that carries a value decides the outcome; a valid
        // query token does not rescue a wrong header token.
        assert_eq!(
            access
                .authorize(&headers, Some("proxy_token=s3cret"))
                .unwrap_err(),
            AccessError::Mismatch
        );
    }

    #[test]
    fn ignores_authorization_and_falls_back_to_query() {
        let access = access(Some("s3cret"), &[]);
        let headers = headers_with("authorization", "Basic dXNlcjpwYXNz");

        // Credentials in `Authorization` always belong to the target, so the
        // proxy token still has to come from another source.
        assert!(access
            .authorize(&headers, Some("proxy_token=s3cret"))
            .is_ok());
    }

    #[test]
    fn empty_allowlist_allows_every_host() {
        let access = access(None, &[]);

        assert!(access.host_allowed(Some("anything.test")));
        assert!(access.host_allowed(None));
    }

    #[test]
    fn allowlist_matches_host_and_subdomains() {
        let access = access(None, &["example.com"]);

        assert!(access.host_allowed(Some("example.com")));
        assert!(access.host_allowed(Some("api.example.com")));
        assert!(access.host_allowed(Some("API.Example.COM.")));
        assert!(!access.host_allowed(Some("example.com.evil.test")));
        assert!(!access.host_allowed(Some("notexample.com")));
        assert!(!access.host_allowed(Some("other.test")));
    }

    #[test]
    fn allowlist_accepts_wildcard_entries_and_allow_all() {
        let wildcard = access(None, &["*.example.com"]);
        assert!(wildcard.host_allowed(Some("api.example.com")));
        assert!(wildcard.host_allowed(Some("example.com")));

        let allow_all = access(None, &["*"]);
        assert!(allow_all.host_allowed(Some("anything.test")));
    }

    #[test]
    fn allowlist_normalizes_entries_and_ignores_blank_ones() {
        let access = access(None, &[" Example.COM. ", "", "  "]);

        assert_eq!(access.allowed_hosts().len(), 1);
        assert_eq!(access.allowed_hosts()[0], "example.com");
        assert!(access.host_allowed(Some("example.com")));
    }

    #[test]
    fn allowlist_handles_ipv6_hosts() {
        let access = access(None, &["::1"]);

        assert!(access.host_allowed(Some("[::1]")));
        assert!(!access.host_allowed(Some("[::2]")));
    }

    #[test]
    fn constant_time_eq_rejects_different_secrets() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
