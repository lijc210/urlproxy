# 部署与镜像发布

本文记录容器化、编排和 CI 发布相关的工程细节。功能与配置项见 [README](../README.md)。

## 镜像构建

`Dockerfile` 是两阶段构建：

| 阶段      | 基础镜像                  | 作用                                                   |
| --------- | ------------------------- | ------------------------------------------------------ |
| `builder` | `rust:1.91-slim-bookworm` | 编译 release 二进制                                    |
| `runtime` | `debian:bookworm-slim`    | 只放二进制 + `ca-certificates` + `curl`（供健康检查）  |

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
# 本地构建并启动
docker compose up -d --build

# 使用 GHCR 上已发布的镜像
export URLPROXY_IMAGE=ghcr.io/<owner>/<repo>:latest
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

`image:` 用的是 `${URLPROXY_IMAGE:-urlproxy:local}`：默认打本地 tag，不去 Docker Hub
猜一个可能不存在的镜像；要消费 GHCR 上的镜像时通过变量覆盖。

编排里还设置了：

- `restart: unless-stopped`
- `healthcheck`（与 Dockerfile 里的一致，`docker compose ps` 能看到 `(healthy)`）
- `logging` 限制 `json-file` 单文件 10MB、保留 3 个，避免 access log 把磁盘写满

## 自动打包（GitHub Packages / GHCR）

`.github/workflows/docker.yml` 分两个 job：

1. `check`：`cargo fmt --all --check`、`cargo clippy --all-targets --locked -- -D warnings`、
   `cargo test --locked`，用 `Swatinem/rust-cache` 缓存。
2. `publish`：`needs: check`，用 buildx 构建并推送多架构镜像，`cache-from/to: type=gha`。

任何一步检查失败都不会发布镜像。

### 触发条件与 tag

| 触发条件                 | 推送的 tag                           |
| ------------------------ | ------------------------------------ |
| push 到 `main`           | `latest`、`main`、`sha-<短 sha>`     |
| push `v1.2.3` 形式的 tag | `1.2.3`、`1.2`、`1`、`sha-<短 sha>`  |
| Pull Request             | 只构建、不推送（tag 为 `pr-<编号>`） |
| 手动 `workflow_dispatch` | 按所在分支的规则                     |

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
- 本仓库当前会同步到 `github.com/lijc210/myscripts`（见 `.cnb/web_trigger.yml`），
  所以发布出来是 `ghcr.io/lijc210/myscripts:latest`。想用别的名字，另建一个 GitHub 仓库即可，
  工作流不用改。
- 工作流假设项目位于仓库根目录。如果同步到某个子目录，需要给两个 job 加
  `working-directory`，并给 `publish` 的 build 加 `context` 指向该子目录。

### 本地验证构建

```bash
docker compose build                        # 等价于校验 Dockerfile
docker buildx build --platform linux/arm64 . # 交叉构建单架构验证
```
