# XiMonitor

XiMonitor 是一个用 Rust 编写的轻量级服务器监控面板，包含：

- `ximonitor-server`
  中心服务，提供 WebSocket 接入、只读页面、只读 JSON API、SQLite 短期历史和快照恢复。
- `ximonitor-agent`
  Linux agent，采集 CPU、负载、内存、磁盘、网络总流量、实时速率和 WebSocket RTT。
- `ximonitor-proto`
  服务端与 agent 共用的配置、协议和数据模型。

## 当前能力

- 服务端只读页面：
  - `/`
  - `/nodes/{node_id}`
- 服务端只读 API：
  - `/api/overview`
  - `/api/nodes`
  - `/api/nodes/{node_id}`
  - `/api/nodes/{node_id}/history`
- agent 接入协议：
  - `hello`
  - `metrics`
  - `ping`
  - `pong`
  - `server_notice`
- 72 小时 SQLite 历史保留
- 快照落盘与进程重启后恢复最近状态
- agent 指数退避自动重连

## 本地构建

```bash
cargo check
```

## 交叉编译 Linux x86_64

仓库内已经包含 musl 目标的 `lld` 链接配置，可以直接在 macOS 上构建静态 Linux `x86_64` 产物：

```bash
cargo build --release --target x86_64-unknown-linux-musl \
  -p ximonitor-server \
  -p ximonitor-agent
```

产物位置：

```bash
target/x86_64-unknown-linux-musl/release/ximonitor-server
target/x86_64-unknown-linux-musl/release/ximonitor-agent
```

这两个产物是：

- ELF 64-bit
- x86-64
- statically linked
- stripped

## 服务端启动

1. 复制配置：

```bash
mkdir -p config
cp config/server.example.toml config/server.toml
```

2. 修改 `config/server.toml` 中的监听地址、`public_base_url`、`node_registry_path`、`[auth]` 用户名密码，以及可选的 `[install].agent_release_base_url`。

3. 准备节点清单文件：

```bash
cp config/server.json.example config/server.json
```

如果你希望从空白清单开始，也可以直接写成：

```json
{
  "nodes": []
}
```

4. 启动服务端：

```bash
cargo run -p ximonitor-server -- --config config/server.toml
```

## 节点签发

推荐先在服务端签发节点，再去目标机器安装 agent。服务端会把节点 token 持久化到 `server.json`，并直接打印可用的安装命令。

```bash
cargo run -p ximonitor-server -- \
  --config config/server.toml \
  issue-node \
  --node-id hk-01 \
  --node-label "Hong Kong 01" \
  --tag apac \
  --tag edge
```

这个命令会：

- 在 `server.json` 里创建或复用 `hk-01`
- 为该节点生成独立 token
- 打印 `agent.toml` 片段
- 打印一条可直接复制到子机执行的安装命令
- 让运行中的服务端在下一次注册表轮询时自动接纳新 token，无需重启进程

注意：

- `/`、`/nodes/*`、`/api/*` 和安装脚本默认受 HTTP Basic Auth 保护
- `issue-node` 打印的安装命令会自动带上 `curl --user ...`，这样子机可以安全地取到安装脚本

如果你需要轮换某个节点 token，可以追加 `--rotate-token`。

## Agent 启动

1. 复制配置：

```bash
cp config/agent.example.toml config/agent.toml
```

2. 把 `node_id`、`node_label`、`server`、`token` 替换成服务端签发输出的内容。

3. 本机采样自检：

```bash
cargo run -p ximonitor-agent -- --config config/agent.toml --sample-once
```

4. 正常运行：

```bash
cargo run -p ximonitor-agent -- --config config/agent.toml
```

## 一键安装

脚本位置：

```bash
scripts/install-agent.sh
```

示例：

```bash
curl -fsSL --user 'viewer:change-this-password' https://monitor.example.com/install/install-agent.sh | sh -s -- \
  --server wss://monitor.example.com/ws \
  --node-id hk-01 \
  --token YOUR_TOKEN \
  --base-url https://downloads.example.com/ximonitor/releases/latest/download
```

说明：

- 脚本会检测架构并下载对应的 `ximonitor-agent-<target>` 二进制
- 会写入 `/etc/ximonitor/agent.toml`，并将权限收紧到 `0600`
- 会生成 `ximonitor-agent.service`
- 会执行 `daemon-reload`、`enable` 和 `restart`

如果你已经有精确二进制地址，也可以改用：

```bash
sh scripts/install-agent.sh \
  --server wss://monitor.example.com/ws \
  --node-id hk-01 \
  --token YOUR_TOKEN \
  --binary-url https://your-host/releases/ximonitor-agent-x86_64-unknown-linux-musl
```

## 说明

- 网页端默认只读，不提供写配置入口。
- `/healthz` 和 `/ws` 不走只读面板鉴权；面板、JSON API 和安装脚本走 HTTP Basic Auth。
- agent 优先使用服务端 `server.json` 中的逐节点 token；旧的 `shared_token` 仅作为兼容迁移手段保留。
- 首版 agent 只支持 Linux。
- 当前历史图保存基础趋势，不做长期归档。
- 生产环境建议放在 Nginx 或 Caddy 后面并启用 HTTPS。
