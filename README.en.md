# urlproxy

English | [简体中文](README.md)

A reverse URL proxy written in Rust: prepend this proxy's address to the target URL and you are done.

```text
http://localhost:3000/https://www.google.com
```

It is the self-hosted equivalent of services like cors-anywhere: use it as an
online page proxy to preview sites, or to let browser code call APIs that do not
send CORS headers.

## Features

- **Forward by path**: the target URL *is* the request path, with full support for
  query strings and request bodies.
- **Streaming in both directions**: request and response bodies are forwarded as
  they arrive, never buffered in memory, so large uploads and downloads are not cut off.
- **CORS handled for you**: responses get the appropriate `Access-Control-*` headers
  and preflight requests are answered by the proxy itself, so even an upstream API
  that knows nothing about CORS works from the browser.
- **Behaves in a real browser**: `3xx` `Location` headers are rewritten, the `Domain`
  attribute is stripped from `Set-Cookie`, and upstream CSP headers are removed, so
  following links and logging in does not break out of the proxy.
- **Optional access token**: set a shared token and only clients that know it can use
  the proxy.
- **Optional host allowlist**: only forward to the domains you list (and their
  subdomains).
- Ships with a `GET /healthz` health check, a usage page at `/`, a Dockerfile,
  a docker-compose file, and multi-arch images.

## Quick start

```bash
cargo run --release
```

Listens on `0.0.0.0:3000` by default. Opening <http://localhost:3000/> shows a small
form you can paste a URL into.

```bash
# Proxy a web page
curl -s http://localhost:3000/https://www.example.com

# Target path and query string are preserved
curl -s "http://localhost:3000/https://httpbin.org/get?a=1&b=2"

# POST bodies and custom headers are forwarded
curl -s -X POST -H 'content-type: application/json' \
  -d '{"hello":"world"}' \
  http://localhost:3000/https://httpbin.org/post
```

### From browser code

```js
const res = await fetch("http://localhost:3000/https://api.example.com/v1/items", {
    method: "POST",
    headers: { "content-type": "application/json", "x-token": "secret" },
    body: JSON.stringify({ name: "demo" }),
});
console.log(await res.json());
```

## Supported path forms

The target must include an explicit `http://` or `https://`; a scheme-less form such
as `/www.google.com` is rejected:

| Request path                            | Proxied to                             |
| --------------------------------------- | -------------------------------------- |
| `/https://www.google.com/search?q=rust` | `https://www.google.com/search?q=rust` |
| `/http://127.0.0.1:8080/api/items`      | `http://127.0.0.1:8080/api/items`      |
| `/https%3A%2F%2Fwww.google.com`         | `https://www.google.com` (percent-encoded) |
| `/www.google.com`                       | `400`, missing scheme                  |
| `/ftp://example.com/a`                  | `400`, only http / https is allowed    |

## Access control

Both features are optional. With neither configured, the proxy requires no
authentication and allows every target host.

### Shared token

Once `PROXY_TOKEN` is set, every request except `GET /` and `GET /healthz` must carry
the token, otherwise the proxy answers `401`. Two ways to send it:

```bash
# 1. Custom header
curl -H 'X-Proxy-Token: s3cret' http://localhost:3000/https://example.com

# 2. Query parameter (for browser address bars and plain link navigation)
curl 'http://localhost:3000/https://example.com?a=1&proxy_token=s3cret'
```

- The `proxy_token` parameter and the `X-Proxy-Token` header are removed before
  forwarding, so the target never sees them.
- The `Authorization` header is **not** a proxy credential. `Bearer` and `Basic` are
  both forwarded to the target untouched, so you cannot authenticate with it.
- CORS preflight requests (`OPTIONS` + `Access-Control-Request-Method`) do not need
  the token, because browsers never send credentials on a preflight.

> With authentication enabled, a target API that has its own `proxy_token` query
> parameter will have it consumed by the proxy as a credential. Avoid that parameter
> name, or pass the token in the header, which has no such restriction.

Sending the token from `fetch`:

```js
await fetch("http://localhost:3000/https://api.example.com/v1/items", {
    headers: { "x-proxy-token": "s3cret" },
});
```

### Host allowlist

`ALLOWED_HOSTS` is a comma-separated list of hostnames. Empty (the default) allows
every host:

```bash
ALLOWED_HOSTS=example.com,api.internal.test ./urlproxy
```

- One entry matches both the domain itself and all of its subdomains: `example.com`
  allows `example.com` and `api.example.com`.
- `*.example.com` means the same thing (and also allows `example.com` itself);
  `*` allows everything.
- Only the hostname is compared. Matching is case-insensitive, ignores a trailing
  dot, and ignores the port.
- Targets outside the allowlist get `403`.

## Configuration

Configured through environment variables:

| Variable          | Default                         | Description                                                                                                                                                                        |
| ----------------- | ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ADDR`            | `0.0.0.0:3000`                  | Listen address                                                                                                                                                                     |
| `PROXY_TOKEN`     | empty (no authentication)       | Shared access token; when set, every proxied request must present it                                                                                                                |
| `ALLOWED_HOSTS`   | empty (all hosts allowed)       | Comma-separated target host allowlist; matches subdomains and supports `*`                                                                                                          |
| `PUBLIC_BASE_URL` | empty                           | Externally visible origin of the proxy, e.g. `https://proxy.example.com`. Needed behind Nginx or a gateway to build redirect URLs; when unset, it is inferred from the request's `Host` / `X-Forwarded-Proto` |
| `RUST_LOG`        | `urlproxy=info,tower_http=info` | Log level                                                                                                                                                                          |

```bash
PROXY_TOKEN=s3cret ALLOWED_HOSTS=example.com,api.internal.test cargo run --release
```

## Running with Docker

```bash
# Build locally and start (maps to port 3000 on the host)
docker compose up -d --build

# Or use the multi-arch image published to GitHub Packages
export URLPROXY_IMAGE=ghcr.io/<github-owner>/<repo>:latest
docker compose pull && docker compose up -d
```

The container runs as a non-root user and ships with a `HEALTHCHECK` (it calls
`GET /healthz`). Settings are passed the same way, via environment variables; they
can also be written to the `.env` file next to `docker-compose.yml`, which compose
loads automatically.

`URLPROXY_IMAGE` is the only variable that selects the image; `PROXY_TOKEN`,
`ALLOWED_HOSTS` and `PUBLIC_BASE_URL` are read from the environment (or `.env`).
See [Deployment and image publishing](docs/deployment.md) (Chinese) for build
details and the GitHub Packages pipeline.

## Security notes

This is an **open proxy with no authentication by default**: anyone who can reach it
can use it to access any `http`/`https` address.

- At the very least, set `PROXY_TOKEN` before exposing it publicly, and restrict
  targets with `ALLOWED_HOSTS` where possible.
- The allowlist compares hostnames only. It does not resolve DNS and cannot stop an
  allowed hostname from pointing at an internal address; add your own IP-level checks
  if you need SSRF protection.
- Request headers are forwarded, including `Cookie` and `Authorization`. Only the
  `X-Proxy-Token` header is removed.
- Tokens passed as a query parameter end up in access logs, browser history and
  server logs. Prefer the header when that matters.

## Documentation

The in-depth documentation is currently written in Chinese:

- [Implementation notes](docs/implementation.md): request handling order, forwarding
  and response rewriting, access-control internals, module layout.
- [Deployment and image publishing](docs/deployment.md): Dockerfile, docker-compose,
  the GHCR workflow and its tag rules.
- [Development and testing](docs/development.md): common commands, test coverage,
  directory layout, conventions.
