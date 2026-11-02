# 开发与测试

本文面向要编译、调试或改动 urlproxy 的开发人员。功能与配置见 [README](../README.md)。

## 环境

- Rust 1.91+（`cargo 1.91.1` 验证过）
- 可选：Docker 29+ / Docker Compose v2（只在验证镜像时需要）

## 常用命令

```bash
cargo run --release      # 本地启动，默认 0.0.0.0:3000
cargo test --locked      # 单元测试
cargo fmt --all          # 格式化
cargo clippy --all-targets --locked -- -D warnings   # 与 CI 一致
cargo build --release    # release 二进制
```

日志级别用 `RUST_LOG` 控制，默认 `urlproxy=info,tower_http=info`。
调试某个请求时可以用：

```bash
RUST_LOG=urlproxy=debug,tower_http=debug cargo run
```

## 测试

`cargo test` 跑的是 `src/**` 里的内联单元测试，共 29 个：

| 模块             | 覆盖内容                                                                                                  |
| ---------------- | --------------------------------------------------------------------------------------------------------- |
| `src/target.rs`  | 协议的必填校验、百分号编码、查询串归属、非法/缺失目标                                                     |
| `src/headers.rs` | 逐跳头与 `Connection` 点名头过滤、默认 UA、`Location` 重写、`Set-Cookie` 去 Domain、CSP 丢弃、CORS 回显   |
| `src/access.rs`  | 两种 token 来源、`Authorization` 不作为凭证、错误分支、查询串剥离、常量时间比较、白名单匹配与规范化、IPv6 |

没有网络相关的集成测试（避免测试依赖外网）。端到端验证靠手工：

```bash
cargo run
curl -s http://127.0.0.1:3000/https://www.example.com | head -5
curl -s -w ' [%{http_code}]\n' http://127.0.0.1:3000/www.google.com   # 期望 400
```

需要验证 POST 请求体、自定义头透传、重定向重写这类行为时，起一个本地回显上游最方便：

```python
# python3 echo_upstream.py 8099
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_POST(self):
        self._echo()
    def do_GET(self):
        self._echo()
    def _echo(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n).decode() if n else ""
        payload = json.dumps({
            "method": self.command,
            "path": self.path,
            "authorization": self.headers.get("authorization"),
            "x_proxy_token": self.headers.get("x-proxy-token"),
            "body": body,
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
```

```bash
# 鉴权 + 白名单 + 透传一起验证
PROXY_TOKEN=s3cret ALLOWED_HOSTS=127.0.0.1 cargo run
curl -s -H 'x-proxy-token: s3cret' -H 'Authorization: Bearer target-secret' -X POST -d 'hi' \
  http://127.0.0.1:3000/http://127.0.0.1:8099/echo
```

回显里的 `authorization` 应该是 `Bearer target-secret`（原样转发），
而 `x_proxy_token` 为 `null`（代理自己的凭证不会发出去）。

## 目录结构

```text
src/main.rs     启动：环境变量、HTTP 客户端、路由、优雅关闭
src/proxy.rs    请求处理：鉴权顺序、转发、流式回写、CORS 预检、错误响应
src/access.rs   token 鉴权与目标域名白名单
src/headers.rs  逐跳头过滤、重定向与 Set-Cookie 重写、CORS 头
src/target.rs   从请求路径解析目标 URL
assets/index.html            根路径的说明页（include_str! 编译进二进制）
Dockerfile                   两阶段构建，依赖层单独缓存
docker-compose.yml           容器编排
.github/workflows/docker.yml CI：检查 + 构建并推送 GHCR
docs/                        实现、部署、开发文档
```

## 改动时的几个约定

- 任何新的响应路径都要经过 `with_cors` / `headers::apply_cors`，
  否则浏览器端拿不到状态码。错误响应也不例外。
- `assets/index.html` 是 `include_str!` 编译进二进制的，改完必须重新 `cargo build`。
- 面向调用方的错误信息用英文（会直接出现在 HTTP 响应体里），代码注释用英文，
  文档用中文。
- 新增请求头处理逻辑时，先确认它是不是逐跳头；不要把 `Host`、`Connection` 之类透传出去。
- 改完跑一遍 `cargo fmt --all --check` 和 `cargo clippy --all-targets -- -D warnings`，
  CI 会把任何告警当成失败。
