//! Request handler: forwards a request to the URL encoded in the path and
//! streams the upstream response back.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode},
};
use reqwest::Url;
use tracing::{debug, warn};

use crate::{access, headers, target, AppState};

const INDEX_HTML: &str = include_str!("../assets/index.html");

type UpstreamError = (StatusCode, String);

pub async fn handle(State(state): State<AppState>, request: Request<Body>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let path = request.uri().path();

    if path == "/" {
        return with_cors(html(INDEX_HTML), origin);
    }

    if path == "/healthz" {
        return with_cors(text(StatusCode::OK, "ok"), origin);
    }

    // Answer CORS preflight before the token check: browsers never send
    // credentials on a preflight request.
    if request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        return preflight(request.headers());
    }

    let authorized = match state
        .access
        .authorize(request.headers(), request.uri().query())
    {
        Ok(authorized) => authorized,
        Err(err) => return with_cors(text(StatusCode::UNAUTHORIZED, &err.to_string()), origin),
    };

    // Rebuild the path from the token-free query so the proxy internals are not
    // sent to the target.
    let mut path_and_query = path.to_owned();
    if let Some(query) = &authorized.query {
        path_and_query.push('?');
        path_and_query.push_str(query);
    }

    let target_url = match target::parse(&path_and_query) {
        Ok(url) => url,
        Err(err) => {
            return with_cors(text(StatusCode::BAD_REQUEST, &err.to_string()), origin);
        }
    };

    if !state.access.host_allowed(target_url.host_str()) {
        let message = format!(
            "target host `{}` is not allowed by ALLOWED_HOSTS",
            target_url.host_str().unwrap_or_default()
        );
        return with_cors(text(StatusCode::FORBIDDEN, &message), origin);
    }

    match forward(&state, request, &target_url).await {
        Ok(response) => response,
        Err((status, message)) => with_cors(text(status, &message), origin),
    }
}

async fn forward(
    state: &AppState,
    request: Request<Body>,
    target: &Url,
) -> Result<Response<Body>, UpstreamError> {
    let (parts, body) = request.into_parts();
    let method = parts.method.clone();

    let mut forwarded_headers = headers::forward_request_headers(&parts.headers);
    // The proxy token is never the target's business. `Authorization` is left
    // alone: it always belongs to the target.
    forwarded_headers.remove(access::TOKEN_HEADER);

    let mut upstream = state
        .client
        .request(method.clone(), target.clone())
        .headers(forwarded_headers);

    if has_body(&parts.headers) {
        // Stream instead of buffering so uploads of any size pass through.
        upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }

    debug!(%method, %target, "proxying request");

    let upstream = upstream.send().await.map_err(|err| {
        warn!(%method, %target, error = %err, "upstream request failed");
        (
            StatusCode::BAD_GATEWAY,
            format!("upstream request failed: {err}"),
        )
    })?;

    let status = upstream.status();
    let proxy_base = proxy_base(&parts.headers, state);
    let response_headers =
        headers::forward_response_headers(upstream.headers(), target, &proxy_base);

    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    headers::apply_cors(response.headers_mut(), parts.headers.get(header::ORIGIN));

    Ok(response)
}

/// The absolute URL of this proxy, used to point rewritten redirects back here.
fn proxy_base(request_headers: &HeaderMap, state: &AppState) -> String {
    if let Some(base) = &state.public_base_url {
        return base.clone();
    }

    let scheme = request_headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("http");

    let host = request_headers
        .get("x-forwarded-host")
        .or_else(|| request_headers.get(header::HOST))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");

    format!("{scheme}://{host}")
}

fn has_body(request_headers: &HeaderMap) -> bool {
    let declared_length = request_headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > 0);

    declared_length || request_headers.contains_key(header::TRANSFER_ENCODING)
}

fn preflight(request_headers: &HeaderMap) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;

    let response_headers = response.headers_mut();
    headers::apply_cors(response_headers, request_headers.get(header::ORIGIN));
    response_headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"),
    );
    response_headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        request_headers
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("*")),
    );
    response_headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );

    response
}

fn with_cors(mut response: Response<Body>, origin: Option<HeaderValue>) -> Response<Body> {
    headers::apply_cors(response.headers_mut(), origin.as_ref());
    response
}

fn text(status: StatusCode, message: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(message.to_owned()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn html(body: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}
