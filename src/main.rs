mod access;
mod headers;
mod proxy;
mod target;

use std::{net::SocketAddr, time::Duration};

use access::Access;
use anyhow::Context;
use axum::Router;
use reqwest::Client;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// State shared by every request.
#[derive(Clone)]
pub struct AppState {
    pub client: Client,
    /// Externally visible origin of this proxy, e.g. `https://proxy.example.com`.
    /// Only needed when running behind another reverse proxy: rewritten
    /// redirects are built from it instead of the incoming `Host` header.
    pub public_base_url: Option<String>,
    /// Optional token authentication and target host allowlist.
    pub access: Access,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("urlproxy=info,tower_http=info")),
        )
        .init();

    let addr: SocketAddr = std::env::var("ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_owned())
        .parse()
        .context("ADDR must be a socket address such as 0.0.0.0:3000")?;

    let public_base_url = std::env::var("PUBLIC_BASE_URL")
        .ok()
        .map(|value| value.trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty());

    // Optional shared token: when set, every proxied request must present it.
    let token = std::env::var("PROXY_TOKEN").ok();
    // Optional comma separated host allowlist; empty means every host is allowed.
    let allowed_hosts: Vec<String> = std::env::var("ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();

    let access = Access::new(token, allowed_hosts);

    if access.token_required() {
        info!("proxy token authentication is enabled");
    } else {
        info!("proxy token authentication is disabled (set PROXY_TOKEN to enable it)");
    }

    if access.allowed_hosts().is_empty() {
        info!("target host allowlist is empty, all hosts are allowed");
    } else {
        info!(
            hosts = access.allowed_hosts().join(", "),
            "target host allowlist is enabled"
        );
    }

    let client = Client::builder()
        // Redirects are rewritten and handed back to the caller instead of being
        // followed here.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_nodelay(true)
        .build()
        .context("failed to build the http client")?;

    let state = AppState {
        client,
        public_base_url,
        access,
    };

    let app = Router::new()
        .fallback(proxy::handle)
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    info!("urlproxy listening on http://{addr}");
    info!("try: http://{addr}/https://www.example.com");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
