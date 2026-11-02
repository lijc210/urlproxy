# 实现说明

本文记录 urlproxy 的内部实现，面向需要改动代码的人。使用方式见 [README](../README.md)。

## 请求处理流程

`src/proxy.rs::handle` 是所有请求的唯一入口（`Router::fallback`），顺序固定：

1. `GET /` 返回使用说明页，`GET /healthz` 返回 `ok`。这两个路径始终公开。
2. 预检请求（`OPTIONS` + 存在 `Access-Control-Request-Method`）在本地直接返回 `204`，
   不再往下走。浏览器不会在预检里带凭证，所以必须在 token 校验之前处理。
3. token 校验，失败返回 `401`。
4. 从路径解析目标 URL，失败返回 `400`。
5. 目标域名白名单校验，失败返回 `403`。
6. 转发请求，上游失败返回 `502`。

这个顺序保证未通过鉴权的调用方拿不到任何目标信息：既不会看到「缺少协议」这类解析结果，
也不会看到白名单的具体配置。

所有错误响应（`400`/`401`/`403`/`502`）都会经过 `with_cors`，补上 CORS 头，
否则浏览器里只能拿到一个不透明的 `TypeError`。

## 目标 URL 解析

实现在 `src/target.rs`。入口是 `parse(path_and_query: &str)`，输入是请求的完整
`path[?query]`（查询串已经在鉴权阶段清理过），处理步骤：

1. 在第一个 `?` 处切分路径和查询串。
2. 只对路径部分做百分号解码，并去掉开头的 `/`。查询串保持原样，避免把目标的
   `a%20b` 变成 `a b`。
3. 要求解码后的字符串以 `http://` 或 `https://` 开头（ASCII 不区分大小写）。
   这是有意的限制：猜协议会在明文和 TLS 之间做一个用户没表达的选择。
4. `Url::parse` 之后校验 host 非空。
5. 如果目标本身没有查询串，就把请求的查询串挂上去。所以
   `/https://host/path?a=1` 代理的是 `https://host/path?a=1`。

百分号编码的整体形式（`/https%3A%2F%2Fhost%2Fpath%3Fa%3D1`）也能解析：
解码后第 3 步拿到的就是带协议的完整 URL，第 5 步因为目标已有查询串而跳过。

## 请求转发

- **流式请求体**：只有当 `content-length > 0` 或存在 `transfer-encoding` 时，
  才用 `reqwest::Body::wrap_stream(body.into_data_stream())` 挂上请求体。
  否则不设置 body，避免给 GET/HEAD 加上 `transfer-encoding: chunked`。
- **逐跳头**：`headers::forward_request_headers` 按 RFC 9110 丢弃 `connection`、
  `keep-alive`、`proxy-authenticate`、`proxy-authorization`、`te`、`trailer`、
  `transfer-encoding`、`upgrade`，以及 `Connection` 头里点名的那些头。
  `Host` 不转发，由 `reqwest` 按目标 URL 重新生成。
- **默认 User-Agent**：请求没有 UA 时补 `urlproxy/<version>`，部分站点会拒绝空 UA。
- **代理凭证剥离**：`X-Proxy-Token` 永远不转发；`Authorization` 不当作代理凭证，
  无论是 `Bearer` 还是 `Basic` 都原样转发给目标。

## 响应回写

- **流式响应体**：`Body::from_stream(upstream.bytes_stream())`，不缓冲；
  `Content-Length`、`Content-Encoding` 原样转发，客户端自己解压。
- **重定向重写**：`reqwest` 关闭了自动跟随（`redirect::Policy::none()`）。
  上游返回 `3xx` 时，用 `target.join(location)` 解析出绝对地址，再拼成
  `{proxy_base}/{absolute}` 返回，浏览器因此始终留在代理下。
  `proxy_base` 优先取 `PUBLIC_BASE_URL`，否则用请求的 `Host` /
  `X-Forwarded-Host` 加 `X-Forwarded-Proto`（缺省 `http`）推断。
- **`Set-Cookie`**：去掉 `Domain` 属性。上游写的域名和代理域名不一致，
  浏览器会直接丢弃这个 Cookie。
- **CSP 剥离**：丢弃 `Content-Security-Policy`、`Content-Security-Policy-Report-Only`
  和 `public-key-pins`。上游按自己的源写策略，在代理源下会让页面加载失败。
- **CORS 覆盖**：先复制上游头，再覆盖 CORS 相关字段，避免上游的
  `Access-Control-Allow-Origin` 和代理的冲突。
    - 请求带 `Origin`：回显该 Origin，`Access-Control-Allow-Credentials: true`，
      并 `append` 一个 `Vary: origin`（用 append 是为了不覆盖上游原有的 `Vary`）。
    - 请求不带 `Origin`：返回 `Access-Control-Allow-Origin: *`。
    - 统一设置 `Access-Control-Expose-Headers: *`。

## 访问控制

实现在 `src/access.rs`，由 `Access::new(token, allowed_hosts)` 构造，
两个能力都可以关闭。

### token

- 来源优先级：`X-Proxy-Token` → `proxy_token` 查询参数。第一个命中的来源决定结果，
  不会继续往下找。
- `Authorization` 头完全不参与鉴权，不论 `Bearer` 还是 `Basic` 都原样转发给目标，
  所以拿它传代理 token 会得到 `401`（`AccessError::Missing`）。
- 比较用 `constant_time_eq`（等长逐字节 XOR 累积），避免按字节短路比较泄露长度相近的猜测。
- 查询参数 `proxy_token` 在鉴权成功后从查询串里剔除（`split_query_token`），
  剔除在 `target::parse` 之前完成，因此不会出现在发给目标的请求里。
- 注意：目标接口自己如果也用 `proxy_token` 这个参数名，开启鉴权后会被消费掉。

### 白名单

- 环境变量里的每一项先规范化：去空白、去末尾的点、转小写、去掉 `*.` 前缀。
- 匹配规则：`host == entry` 或 `host.ends_with(".{entry}")`，即同时覆盖自身和子域名。
- 条目 `*` 表示放行全部；列表为空也等于放行全部。
- 比较用的 host 会去空白、去末尾的点、去 IPv6 的方括号，并转小写。
  `host_str()` 不含端口，所以白名单不区分端口。
- 白名单只校验请求头里的目标域名，不做 DNS 解析，也不做 IP 级校验；
  需要防 SSRF 时要另外实现。

## 模块结构

| 路径                | 职责                                                    |
| ------------------- | ------------------------------------------------------- |
| `src/main.rs`       | 读取环境变量、构建 HTTP 客户端、路由、日志、优雅关闭    |
| `src/proxy.rs`      | 请求处理：鉴权顺序、转发、流式回写、CORS 预检、错误响应 |
| `src/access.rs`     | token 鉴权（两种来源、常量时间比较）与目标域名白名单    |
| `src/headers.rs`    | 逐跳头过滤、`Location` 重写、`Set-Cookie` 重写、CORS 头 |
| `src/target.rs`     | 从请求路径解析目标 URL，强制显式 `http(s)://`           |
| `assets/index.html` | 根路径的说明页，`include_str!` 编译进二进制             |

`AppState` 持有 `reqwest::Client`（连接池复用）、`public_base_url` 和 `Access`，
通过 `axum::extract::State` 注入 handler。

### reqwest 客户端配置

```rust
.redirect(reqwest::redirect::Policy::none()) // 重定向自己重写
.connect_timeout(Duration::from_secs(10))
.read_timeout(Duration::from_secs(30))       // 单次读超时，不是整个响应
.pool_idle_timeout(Duration::from_secs(30))
.tcp_nodelay(true)
```

刻意没有设置整体 `timeout`：那会限制整个响应体的传输时长，大文件下载会被截断。
`read_timeout` 只约束「多久没收到数据」，慢但持续的下载不受影响。

TLS 使用 `rustls-tls`（webpki 根证书），因此运行镜像不需要 OpenSSL，也不依赖系统 CA。
