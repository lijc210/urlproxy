# 部署与镜像发布

本文记录容器化、编排和 CI 发布相关的工程细节。功能与配置项见 [README](../README.md)。

## 镜像构建

`Dockerfile` 是两阶段构建：

| 阶段      | 基础镜像                  | 作用                                                  |
| --------- | ------------------------- | ----------------------------------------------------- |
| `builder` | `rust:1.91-slim-bookworm` | 编译 release 二进制                                   |
| `runtime` | `debian:bookworm-slim`    | 只放二进制 + `ca-certificates` + `curl`（供健康检查） |

依赖层单独缓存的做法：

```dockerfile
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked

COPY src ./src
COPY assets ./assets
RUN touch src/main.rs && cargo build --release --locked
```

先用一个空的 `main.rs` 把所有依赖编译一遍，再拷真实源码。这样只改业务代码时，
`cargo` 只需要重编译本项目这一个 crate。`touch` 是必需的：`COPY` 会保留文件原本的
时间戳，如果比构建产物旧，`cargo` 会误判为不需要重编译，留下那个空 main 的二进制。

其它细节：

- `--locked` 保证按 `Cargo.lock` 构建，锁文件与 `Cargo.toml` 不一致时直接失败。
- 运行镜像里用 `useradd` 创建 `uid=10001` 的 `urlproxy` 用户，容器以非 root 运行。
- `HEALTHCHECK` 用 `curl -fsS http://127.0.0.1:3000/healthz`，所以运行镜像里装了 `curl`。
  这会让镜像大约 120MB，换来开箱可用的健康检查和容器内排查能力。
- `.dockerignore` 排除了 `target/`、`.git/`、`.env` 等，避免把构建产物和密钥传进 build context。
- 因为用的是 `rustls` + webpki 根证书，运行阶段不依赖 OpenSSL 和系统 CA。

## docker-compose

```bash
# 本地构建并启动（用 compose 里的 build 配置构建，并打上 image: 里的 tag）
docker compose up -d --build

# 或直接拉取 GHCR 上已发布的多架构镜像
docker compose pull && docker compose up -d
```

`docker-compose.yml` 通过环境变量透传配置，`${VAR:-}` 写法让这些值可以来自 shell，
也可以写进同目录的 `.env`（compose 会自动加载）：

```yaml
environment:
  ADDR: 0.0.0.0:3000
  RUST_LOG: urlproxy=info,tower_http=info
  PROXY_TOKEN: ${PROXY_TOKEN:-}
  ALLOWED_HOSTS: ${ALLOWED_HOSTS:-}
  PUBLIC_BASE_URL: ${PUBLIC_BASE_URL:-}
```

`image:` 写的是固定名字 `ghcr.io/lijc210/urlproxy:latest`，和 workflow 里的
`ghcr.io/${{ github.repository }}` 对应：只有 GitHub 仓库叫 `urlproxy` 时两者才一致，
所以发布前要确认仓库名；不一致就改 compose 里的 `image:` 一行。
本地的 `docker compose up -d --build` 会把这个 tag 指向本地构建的镜像，
所以不会去拉一个可能不存在的远程镜像。

编排里还设置了：

- `restart: unless-stopped`
- `healthcheck`（与 Dockerfile 里的一致，`docker compose ps` 能看到 `(healthy)`）
- `logging` 限制 `json-file` 单文件 10MB、保留 3 个，避免 access log 把磁盘写满

## 自动打包（GitHub Packages / GHCR）

`.github/workflows/docker.yml` 分三个 job：

1. `check`：`cargo fmt --all --check`、`cargo clippy --all-targets --locked -- -D warnings`、
   `cargo test --locked`，用 `Swatinem/rust-cache` 缓存。
2. `build`：matrix 两个条目，`linux/amd64` 跑在 `ubuntu-24.04`、`linux/arm64` 跑在
   `ubuntu-24.04-arm`，各自在自己的原生 CPU 上编译，然后按 digest 推送单平台镜像
   （`push-by-digest=true,name-canonical=true`），GHA 缓存用 `scope=<arch>` 分开存。
3. `merge`：`needs: [check, build]`，下载各平台的 digest，用
   `docker buildx imagetools create` 合成 manifest list 并打上 tag。

`check` 和 `build` 并行执行，`merge` 在两者都成功后才发布，
所以检查失败的提交不会产生带 tag 的镜像。PR 只跑 `check` 和两个平台的 `build`（不推送）。

### 为什么不用 QEMU

早期版本用 `platforms: linux/amd64,linux/arm64` 配合 `setup-qemu-action`，在 x86 runner
上模拟构建 arm64。Rust 编译是 CPU 密集型负载，在 QEMU 用户态模拟下会慢 5～15 倍，
冷缓存时一次构建要 15～30 分钟。现在 arm64 用 GitHub 的原生 arm64 runner，
两个平台都是原生编译，同样的流水线通常 3～5 分钟跑完。

前提：原生 arm64 runner 对 **public 仓库**免费；private 仓库使用 `ubuntu-24.04-arm`
会计费。不想承担这部分成本的话，把 matrix 里 arm64 那一条删掉即可，只发 amd64
（Apple Silicon 上的 Docker Desktop 可以用 Rosetta 运行 amd64 镜像）。

### 为什么按 digest 分开推送

多架构镜像不能在两个 runner 上各自打同一个 tag（后推送的会覆盖前一个）。
所以每个平台只推送一个不带 tag 的 digest，再由 `merge` job 把两个 digest
合成 manifest list，最后才打上 `latest` / `1.2.3` 这些 tag。
这也意味着 `merge` 失败时 registry 里只会留下两个没人引用的 digest，不会污染已有 tag。

### 触发条件与 tag

| 触发条件                 | 推送的 tag                          |
| ------------------------ | ----------------------------------- |
| push 到 `main`           | `latest`、`main`、`sha-<短 sha>`    |
| push `v1.2.3` 形式的 tag | `1.2.3`、`1.2`、`1`、`sha-<短 sha>` |
| Pull Request             | 只构建两个平台、不推送              |
| 手动 `workflow_dispatch` | 按所在分支的规则                    |

PR 触发带了 `paths` 过滤，只有 Dockerfile、compose、依赖清单、`src/`、`assets/` 或
工作流本身变化时才跑。

### 发布一个新的版本

```bash
git tag v1.2.3
git push origin v1.2.3
```

会自动产出 `ghcr.io/<owner>/<repo>:1.2.3`、`:1.2`、`:1` 等 tag。

### 镜像名与权限

- 镜像名是 `ghcr.io/${{ github.repository }}`，由 `docker/metadata-action` 生成并自动转小写，
  仓库名含大写也没问题。
- 工作流里已声明 `permissions: packages: write`，不需要额外改仓库的 Workflow permissions。
- 首次发布后包默认是 private，别人 `docker compose pull` 前需要 `docker login ghcr.io`；
  想让镜像公开就到仓库的 Packages 页面改可见性。
- 本仓库当前会同步到 GitHub，镜像名随仓库名走。按 compose 里的配置，
  GitHub 仓库应该命名为 `urlproxy`，发布出来才是 `ghcr.io/lijc210/urlproxy:latest`。
  仓库名不同也可以，改 compose 里的 `image:` 一行即可，工作流不用动。
- 工作流假设项目位于仓库根目录。如果同步到某个子目录，需要给 `check` / `build` / `merge`
  加 `working-directory`，并给 `build` 的 build 加 `context` 指向该子目录。

### 本地验证构建

```bash
docker compose build                          # 等价于校验 Dockerfile
docker buildx build --platform linux/arm64 .   # 单独验证 arm64（Apple Silicon 上是原生构建）
```

## 前置反向代理（Nginx / CDN）

代理生成重定向地址时需要知道自己的对外地址。设置 `PUBLIC_BASE_URL`（例如
`https://proxy.example.com`），或者让前置代理传 `Host` 和 `X-Forwarded-Proto`。

Nginx 默认开启 `merge_slashes`，会把路径里的 `//` 合并成一个 `/`，于是
`/https://example.com` 到达应用时已经变成 `/https:/example.com`。应用会把这个位置
缺掉的一个斜杠补回来，所以默认配置下也能用；要完全按原样转发可以关掉：

```nginx
location / {
    merge_slashes off;                    # nginx 1.13.0+
    proxy_buffering off;                  # 保持流式，SSE 之类才不会被攒住
    proxy_pass http://127.0.0.1:3000;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

`merge_slashes off` 是 nginx 1.13.0 才有的指令。即使关掉，也建议在需要精确保留
路径里 `//` 时使用百分号编码的形式（`/https%3A%2F%2F...`），因为有些 CDN 并不
提供关闭选项。
