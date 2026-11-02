//! Header plumbing for the proxy.
//!
//! Only end-to-end headers are forwarded. Per RFC 9110 the hop-by-hop ones
//! (plus any header nominated by `Connection`) must not be relayed.

use axum::http::{header, HeaderMap, HeaderName, HeaderValue};
use reqwest::Url;

const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Response headers that are meaningless once the body is served from the proxy
/// origin, and that would break pages loaded through it.
const DROPPED_RESPONSE_HEADERS: [&str; 3] = [
    "content-security-policy",
    "content-security-policy-report-only",
    "public-key-pins",
];

/// Builds the header set sent upstream: everything except hop-by-hop headers
/// and `Host`, which is derived from the target URL by the client.
pub fn forward_request_headers(incoming: &HeaderMap) -> HeaderMap {
    let connection_tokens = connection_tokens(incoming);

    let mut out = HeaderMap::with_capacity(incoming.len());
    for (name, value) in incoming {
        if name == header::HOST || !is_end_to_end(name, &connection_tokens) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }

    // Some sites reject requests without a `User-Agent`.
    if !out.contains_key(header::USER_AGENT) {
        out.insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("urlproxy/", env!("CARGO_PKG_VERSION"))),
        );
    }

    out
}

/// Builds the header set returned to the caller, rewriting everything that
/// points at the upstream origin.
pub fn forward_response_headers(upstream: &HeaderMap, target: &Url, proxy_base: &str) -> HeaderMap {
    let connection_tokens = connection_tokens(upstream);

    let mut out = HeaderMap::with_capacity(upstream.len());
    for (name, value) in upstream {
        if !is_end_to_end(name, &connection_tokens) || is_dropped_response_header(name) {
            continue;
        }

        match name.as_str() {
            "location" => {
                if let Some(rewritten) = rewrite_location(value, target, proxy_base) {
                    out.append(name.clone(), rewritten);
                }
            }
            "set-cookie" => {
                if let Some(rewritten) = strip_cookie_domain(value) {
                    out.append(name.clone(), rewritten);
                }
            }
            _ => {
                out.append(name.clone(), value.clone());
            }
        }
    }

    out
}

/// Adds permissive CORS headers so browser code can call the proxy directly.
pub fn apply_cors(response_headers: &mut HeaderMap, request_origin: Option<&HeaderValue>) {
    match request_origin {
        Some(origin) => {
            response_headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            response_headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
            // The response depends on `Origin`, so caches must key on it.
            response_headers.append(header::VARY, HeaderValue::from_static("origin"));
        }
        None => {
            response_headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
        }
    }

    response_headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("*"),
    );
}

fn is_end_to_end(name: &HeaderName, connection_tokens: &[String]) -> bool {
    !HOP_BY_HOP.iter().any(|hop| name.as_str() == *hop)
        && !connection_tokens.iter().any(|token| token == name.as_str())
}

fn is_dropped_response_header(name: &HeaderName) -> bool {
    DROPPED_RESPONSE_HEADERS
        .iter()
        .any(|dropped| name.as_str() == *dropped)
}

/// Collects the header names nominated by the `Connection` header, e.g.
/// `Connection: keep-alive, X-Custom` makes `X-Custom` hop-by-hop as well.
fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

/// Redirects are resolved against the upstream URL and then pointed back at the
/// proxy, so the browser never leaves it.
fn rewrite_location(value: &HeaderValue, target: &Url, proxy_base: &str) -> Option<HeaderValue> {
    let raw = value.to_str().ok()?;
    let absolute = target.join(raw).ok()?;

    if !matches!(absolute.scheme(), "http" | "https") {
        return None;
    }

    HeaderValue::from_str(&format!("{proxy_base}/{absolute}")).ok()
}

/// Cookies set by the upstream carry its own `Domain`, which browsers reject
/// for the proxy host, so the attribute is removed.
fn strip_cookie_domain(value: &HeaderValue) -> Option<HeaderValue> {
    let raw = value.to_str().ok()?;

    let cleaned = raw
        .split(';')
        .filter(|attribute| {
            let attribute = attribute.trim_start();
            !attribute
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("domain="))
        })
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ");

    if cleaned.is_empty() {
        return None;
    }

    HeaderValue::from_str(&cleaned).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_hop_by_hop_and_connection_nominated_headers() {
        let mut incoming = HeaderMap::new();
        incoming.insert(header::HOST, HeaderValue::from_static("proxy.local"));
        incoming.insert(header::CONNECTION, HeaderValue::from_static("x-drop-me"));
        incoming.insert("x-drop-me", HeaderValue::from_static("1"));
        incoming.insert(header::TE, HeaderValue::from_static("trailers"));
        incoming.insert(header::USER_AGENT, HeaderValue::from_static("curl/8"));
        incoming.insert(header::ACCEPT, HeaderValue::from_static("*/*"));

        let out = forward_request_headers(&incoming);

        assert!(!out.contains_key(header::HOST));
        assert!(!out.contains_key("x-drop-me"));
        assert!(!out.contains_key(header::TE));
        assert_eq!(out.get(header::USER_AGENT).unwrap(), "curl/8");
        assert_eq!(out.get(header::ACCEPT).unwrap(), "*/*");
    }

    #[test]
    fn adds_default_user_agent() {
        let out = forward_request_headers(&HeaderMap::new());
        assert_eq!(out.get(header::USER_AGENT).unwrap(), "urlproxy/0.1.0");
    }

    #[test]
    fn rewrites_absolute_and_relative_locations() {
        let target = Url::parse("https://example.com/a/b").unwrap();
        let base = "http://proxy.local";

        assert_eq!(
            rewrite_location(&HeaderValue::from_static("/login"), &target, base).unwrap(),
            "http://proxy.local/https://example.com/login"
        );
        assert_eq!(
            rewrite_location(
                &HeaderValue::from_static("https://other.test/x?y=1"),
                &target,
                base
            )
            .unwrap(),
            "http://proxy.local/https://other.test/x?y=1"
        );
    }

    #[test]
    fn tolerates_unparsable_location() {
        let target = Url::parse("https://example.com/").unwrap();
        assert!(rewrite_location(&HeaderValue::from_static("http://["), &target, "b").is_none());
    }

    #[test]
    fn strips_cookie_domain_only() {
        assert_eq!(
            strip_cookie_domain(&HeaderValue::from_static(
                "session=abc; Domain=.example.com; Path=/; HttpOnly"
            ))
            .unwrap(),
            "session=abc; Path=/; HttpOnly"
        );
        assert_eq!(
            strip_cookie_domain(&HeaderValue::from_static("a=1; dOmAiN=x.test")).unwrap(),
            "a=1"
        );
    }

    #[test]
    fn drops_csp_from_responses() {
        let mut upstream = HeaderMap::new();
        upstream.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'"),
        );
        upstream.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));

        let out = forward_response_headers(
            &upstream,
            &Url::parse("https://example.com/").unwrap(),
            "http://proxy.local",
        );

        assert!(!out.contains_key(header::CONTENT_SECURITY_POLICY));
        assert_eq!(out.get(header::CONTENT_TYPE).unwrap(), "text/html");
    }

    #[test]
    fn echoes_origin_for_cors() {
        let mut headers = HeaderMap::new();
        apply_cors(
            &mut headers,
            Some(&HeaderValue::from_static("https://app.test")),
        );

        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "https://app.test"
        );
        assert_eq!(
            headers
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .unwrap(),
            "true"
        );
    }
}
