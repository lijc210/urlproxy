# urlproxy

简体中文 | [English](README.en.md)

一个用 Rust 实现的反向 URL 代理：把目标地址直接拼在代理地址后面即可访问。

```text
http://localhost:3000/https://www.google.com
```

它本质上就是 cors-anywhere 那类工具的自建版本：既能当「在线网页代理」预览页面，
也能让浏览器里的前端代码跨域调用没有配置 CORS 的接口。

## 功能

- **按路径转发**：目标地址就是请求路径，支持任意 `http` / `https` 地址、查询串和请求体。
- **流式双向转发**：请求体和响应体都边收边发，不在内存里缓冲，大文件上传下载不会被截断。
- **补 CORS**：响应自动加上适当的 `Access-Control-*` 头，预检请求在代理本地直接回答，
  所以上游接口完全不懂 CORS 也能被浏览器调用。
- **浏览器里能正常用**：重写 `3xx` 的 `Location`、去掉 `Set-Cookie` 的 `Domain`、
  剥离上游的 CSP，跟随链接和登录都不会跳出代理。
- **可选访问 token**：设置一个共享 token 后，不知道 token 的人无法使用代理。
- **可选域名白名单**：只允许转发到指定域名（及其子域名）。
- 自带健康检查 `GET /healthz` 和根路径说明页，附带 Dockerfile / docker-compose / 多架构镜像。

## 快速开始

```bash
cargo run --release
```

默认监听 `0.0.0.0:3000`，打开 <http://localhost:3000/> 有一个输入框页面。

```bash
# 代理一个网页
curl -s http://localhost:3000/https://www.example.com

# 保留目标的路径和查询参数
curl -s "http://localhost:3000/https://httpbin.org/get?a=1&b=2"

# 转发 POST 请求体和自定义请求头
curl -s -X POST -H 'content-type: application/json' \
  -d '{"hello":"world"}' \
  http://localhost:3000/https://httpbin.org/post
```

### 在浏览器前端里使用

```js
const res = await fetch("http://localhost:3000/https://api.example.com/v1/items", {
    method: "POST",
    headers: { "content-type": "application/json", "x-token": "secret" },
    body: JSON.stringify({ name: "demo" }),
});
console.log(await res.json());
```

## 支持的路径形式

目标地址必须带 `http://` 或 `https://`，省略协议的写法（如 `/www.google.com`）会被拒绝：

| 请求路径                                | 代理到                                 |
| --------------------------------------- | -------------------------------------- |
| `/https://www.google.com/search?q=rust` | `https://www.google.com/search?q=rust` |
| `/http://127.0.0.1:8080/api/items`      | `http://127.0.0.1:8080/api/items`      |
| `/https%3A%2F%2Fwww.google.com`         | `https://www.google.com`（百分号编码） |
| `/www.google.com`                       | `400`，缺少协议                        |
| `/ftp://example.com/a`                  | `400`，只允许 http / https             |

## 访问控制

两项都是可选的，不配置时无鉴权、允许所有域名。

### 共享 token

设置 `PROXY_TOKEN` 后，除 `GET /` 和 `GET /healthz` 外的请求都必须带上 token，
否则返回 `401`。支持两种传递方式：

```bash
# 1. 自定义头
curl -H 'X-Proxy-Token: s3cret' http://localhost:3000/https://example.com

# 2. 查询参数（浏览器地址栏、链接跳转时用这个）
curl 'http://localhost:3000/https://example.com?a=1&proxy_token=s3cret'
```

- `proxy_token` 参数和 `X-Proxy-Token` 头在转发前会被移除，不会发给目标。
- `Authorization` 头**不作为**代理凭证，无论 `Bearer` 还是 `Basic` 都原样转发给目标，
  所以用 `Authorization` 传 token 是行不通的。
- 预检请求（`OPTIONS` + `Access-Control-Request-Method`）不需要 token，
  因为浏览器不会在预检里发送凭证。

> 开启鉴权后，目标接口自己的 `proxy_token` 查询参数会被代理当作凭证消耗掉，
> 请避开这个参数名；用请求头传 token 则没有这个限制。

浏览器 `fetch` 的写法：

```js
await fetch("http://localhost:3000/https://api.example.com/v1/items", {
    headers: { "x-proxy-token": "s3cret" },
});
```

### 域名白名单

`ALLOWED_HOSTS` 是逗号分隔的域名列表，为空（默认）时允许所有域名：

```bash
ALLOWED_HOSTS=example.com,api.internal.test ./urlproxy
```

- 一项同时匹配该域名本身和它的所有子域名，`example.com` 会放行 `example.com` 和 `api.example.com`。
- 写成 `*.example.com` 效果相同（也会放行 `example.com` 本身），`*` 表示允许全部。
- 只比较主机名，不区分大小写，忽略末尾的点，也不看端口。
- 不在白名单内的目标返回 `403`。

## 配置

通过环境变量配置：

| 变量              | 默认值                          | 说明                                                                                                                                                                |
| ----------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ADDR`            | `0.0.0.0:3000`                  | 监听地址                                                                                                                                                            |
| `PROXY_TOKEN`     | 空（不鉴权）                    | 共享访问 token，设置后所有代理请求必须携带                                                                                                                          |
| `ALLOWED_HOSTS`   | 空（允许所有域名）              | 逗号分隔的目标域名白名单，支持子域名匹配和 `*`                                                                                                                      |
| `PUBLIC_BASE_URL` | 空                              | 对外可见的代理地址，例如 `https://proxy.example.com`。当代理本身部署在 Nginx/网关后面时，用它来生成重定向地址；不设置则根据请求的 `Host` / `X-Forwarded-Proto` 推断 |
| `RUST_LOG`        | `urlproxy=info,tower_http=info` | 日志级别                                                                                                                                                            |

```bash
PROXY_TOKEN=s3cret ALLOWED_HOSTS=example.com,api.internal.test cargo run --release
```

## 用 Docker 运行

```bash
# 本地构建并启动（默认映射到宿主机的 3000 端口）
docker compose up -d --build

# 或者直接使用 GitHub Packages 上已发布的多架构镜像
docker compose pull && docker compose up -d
```

容器以非 root 用户运行，自带 `HEALTHCHECK`（请求 `GET /healthz`），
配置项同样通过环境变量传入（可写在 `.env` 里）。

`docker-compose.yml` 里的 `image:` 是固定值 `ghcr.io/lijc210/urlproxy:latest`，
和 GitHub 仓库名对应；仓库不叫 `urlproxy` 的话改这一行即可。

镜像构建、compose 细节、GitHub Packages 自动打包流程见
[部署与镜像发布](docs/deployment.md)。

## 安全提示

这是一个**默认无鉴权的开放代理**：任何人都能借它访问任意 `http`/`https` 地址。

- 公开部署时至少设置 `PROXY_TOKEN`，并按需用 `ALLOWED_HOSTS` 限制目标域名。
- 白名单只按主机名判断，不解析 DNS，也不能阻止白名单域名指向内网地址；需要防 SSRF 时请自行再加 IP 级校验。
- 代理会透传请求头（包括 `Cookie` 和 `Authorization`）；只有 `X-Proxy-Token` 头会被移除。
- 用查询参数传 token 时，token 会出现在访问日志、浏览器历史和服务端日志里，安全要求高时优先用请求头。

## 文档

- [实现说明](docs/implementation.md)：请求处理顺序、转发与响应回写的实现、访问控制的内部逻辑、模块结构。
- [部署与镜像发布](docs/deployment.md)：Dockerfile、docker-compose、GHCR 自动打包与 tag 规则。
- [开发与测试](docs/development.md)：常用命令、测试覆盖、目录结构、改动约定。
