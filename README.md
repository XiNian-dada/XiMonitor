![NodeLite Banner](images/Banner.png)

[![CI](https://github.com/XiNian-dada/NodeLite/actions/workflows/ci.yml/badge.svg)](https://github.com/XiNian-dada/NodeLite/actions/workflows/ci.yml)
[![Coverage](https://github.com/XiNian-dada/NodeLite/actions/workflows/coverage.yml/badge.svg)](https://github.com/XiNian-dada/NodeLite/actions/workflows/coverage.yml)

# NodeLite

NodeLite 是一个用 Rust 编写的轻量级服务器监控面板，包含：

- `nodelite-server`
  中心服务，提供 WebSocket 接入、只读页面、只读 JSON API、SQLite 短期历史和快照恢复。
- `nodelite-agent`
  Linux agent，采集 CPU、负载、内存、磁盘、网络总流量、实时速率和 WebSocket RTT。
- `nodelite-proto`
  服务端与 agent 共用的配置、协议和数据模型。

适合这样的场景：

- 想要一个像哪吒面板那样“服务端 + 一条命令安装 Agent”的监控系统
- 想把面板本体做得尽量轻，优先稳定、低占用、易部署
- 希望网页端以查看为主，敏感配置仍然落在服务端文件与受控入口

## 快速开始

推荐按下面 3 步体验完整流程：

1. 安装服务端

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | sudo sh
```

2. 在服务端签发一条子机安装命令

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  install-agent \
  --node-id hk-01 \
  --node-label "Hong Kong 01"
```

3. 把上一步打印出的命令粘贴到目标 Linux 子机执行

安装成功后：

- 面板通过 `https://你的域名/` 访问
- Agent 通过 `wss://你的域名/ws` 自动接入
- 历史数据默认保留 14 天

## 支持平台

- `nodelite-server`
  推荐部署在 Linux（systemd 环境），官方发布产物提供 `x86_64-unknown-linux-musl` 与 `aarch64-unknown-linux-musl`
- `nodelite-agent`
  当前仅支持 Linux
- 反向代理
  推荐使用 Nginx 或 Caddy 终结 HTTPS / WSS

## 截图展示

### 首页
![首页](images/home_page.png)

### 节点详情
![详情页面](images/details_page.png)

### 硬件信息
![硬件页面](images/hardware_page.png)

### 网络监控
![网络页面](images/network_page.png)

### 实时监控
![监控页面](images/monitor_page.png)

### 系统设置
![设置页面](images/settings_page.png)

## 性能表现

在 4 个被控端的实际运行环境下：

- **服务端内存占用**：4-10 MB
- **Agent 内存占用**：800-1000 KB

得益于 Rust 的零成本抽象和高效内存管理，NodeLite 在保持功能完整的同时实现了极低的资源占用。

### 压测基线

下面这组数据来自仓库内置的真实 loopback 压测：

```bash
cargo test -p nodelite-server load_test_scaling_scores -- --ignored --nocapture
```

测试机为 `Apple M1 Pro / 32 GB`，通过真实 WebSocket 建链、真实 `metrics` 上报和真实 `/api/overview` 轮询得到以下基线成绩：

| 节点数 | 全部接入耗时 | 收敛耗时 | 总 metrics 数 | metrics 吞吐 | overview p50 | overview p95 | overview max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 20 | 4.4 ms | 20.9 ms | 240 | 11461.3/s | 0.70 ms | 1.20 ms | 1.74 ms |
| 50 | 14.3 ms | 43.3 ms | 600 | 13860.1/s | 1.22 ms | 1.73 ms | 4.50 ms |
| 100 | 9.8 ms | 88.1 ms | 1200 | 13617.9/s | 0.94 ms | 6.77 ms | 7.77 ms |
| 200 | 36.9 ms | 128.5 ms | 2400 | 18677.0/s | 1.02 ms | 3.81 ms | 12.17 ms |

说明：

- 这里的“接入耗时”指批量节点建立 WebSocket 并完成认证的时间。
- “收敛耗时”指最后一轮指标上报与面板总览 API 延迟稳定下来的时间。
- 这组成绩主要用于公开展示项目的量级感，不等同于生产 SLA。
- 实际表现会受到构建模式、反向代理、SQLite I/O、历史保留时长和宿主机网络条件影响。

## 当前能力

- 一键安装与升级：
  - `install-server.sh`
  - `install-agent.sh`
  - `nodelite-server install-agent`
  - `nodelite-server upgrade-agent`
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
  - `refresh_token_request`
  - `refresh_token_response`
- 设置页：
  - 查看服务端版本、路径、历史保留与节点 token 到期时间
  - 修改只读面板密码
  - 启用 / 禁用 TOTP 2FA
  - 手动触发服务端更新并查看更新日志
- 14 天 SQLite 历史保留
- 快照落盘与进程重启后恢复最近状态
- agent 指数退避自动重连

## 本地构建

```bash
cargo check
```

## 测试覆盖率

安装 [cargo-tarpaulin](https://github.com/xd009642/tarpaulin)(仅需一次,仅支持 Linux):

```bash
cargo install cargo-tarpaulin
```

运行覆盖率分析:

```bash
cargo tarpaulin --config tarpaulin.toml
```

HTML 报告输出到 `target/tarpaulin/tarpaulin-report.html`,可用浏览器打开查看逐行覆盖情况。

### 覆盖率目标

| 阶段 | 目标 | 时间 |
| --- | --- | --- |
| 基线 | 记录当前值 | 立即 |
| 短期 | 75% | 2 周内 |
| 长期 | 80% | 持续 |

重点覆盖模块:auth、admission、sanitize、registry(安全关键路径)。

## 交叉编译 Linux x86_64 / aarch64

仓库内已经包含 musl 目标的 `lld` 链接配置，可以直接构建静态 Linux 二进制：

```bash
cargo build --release --target x86_64-unknown-linux-musl \
  -p nodelite-server \
  -p nodelite-agent

cargo build --release --target aarch64-unknown-linux-musl \
  -p nodelite-server \
  -p nodelite-agent
```

产物位置：

```bash
target/x86_64-unknown-linux-musl/release/nodelite-server
target/x86_64-unknown-linux-musl/release/nodelite-agent
target/aarch64-unknown-linux-musl/release/nodelite-server
target/aarch64-unknown-linux-musl/release/nodelite-agent
```

## 推荐部署拓扑

生产环境建议这样放：

1. `nodelite-server` 监听在 `127.0.0.1:8080`
2. Nginx 或 Caddy 对外暴露 `443`
3. 面板和 API 走 HTTPS
4. Agent 通过 `wss://你的域名/ws` 接入

这样可以把 TLS、访问日志、限流和基础访问控制都放到反代层。

## 服务端部署

推荐直接用 GitHub Release 里的交互式安装器。它会清屏、询问安装目录、监听端口、对外域名或 IP、只读面板账号密码，然后自动：

- 按当前架构下载最新的 `nodelite-server`
- 拉取 `SHA256SUMS.txt` 并校验二进制
- 生成 `server.toml` 和 `server.json`
- 注册并启动 `nodelite-server.service`

一条命令安装：

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | sudo sh
```

同一条命令以后也可以直接拿来升级。脚本会自动识别现有安装，并默认切到 `upgrade` 模式；如果你想强制指定，也可以：

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | \
  sudo NODELITE_SERVER_MODE=upgrade sh
```

脚本默认会：

- 把程序数据放到你输入的安装目录下，默认建议 `/opt/nodelite`
- 监听在 `127.0.0.1:<随机端口>`
- 要求你输入对外访问的域名或 IP，并据此生成 `public_base_url`
- 生成一组只读面板 Basic Auth 账号
- 生成带 `NoNewPrivileges`、`ProtectSystem`、`ProtectKernel*`、`CapabilityBoundingSet=` 等限制的 systemd service
- 如果是升级，只替换二进制与 systemd unit，保留现有 `server.toml`、面板账号密码、节点 token 和注册表内容不变

安装完成后会直接打印：

- 服务端二进制路径
- 配置文件路径
- 节点注册表路径
- 面板只读用户名和密码
- 下一条该执行的 `install-agent` 签发命令

如果你更想手工部署，也可以：

1. 复制 [config/server.example.toml](config/server.example.toml) 和 [config/server.json.example](config/server.json.example)
2. 把服务端二进制安装到 `/usr/local/bin/nodelite-server`
3. 手工创建 systemd unit
4. 启动 `nodelite-server.service`

最少要确认的配置项是：

```toml
[server]
listen = "127.0.0.1:28080"
public_base_url = "https://monitor.example.com"
node_registry_path = "/opt/nodelite/config/server.json"
history_db_path = "/opt/nodelite/data/history.sqlite3"
snapshot_path = "/opt/nodelite/data/snapshot.json"

[auth]
username = "viewer"
password = "change-this-password"
# 默认关闭;开启后必须配置 totp_secret。
enable_2fa = false
# totp_secret = "JBSWY3DPEHPK3PXP"

[ws]
max_total_connections = 1024
max_connections_per_ip = 32
auth_fail_window_secs = 300
auth_fail_max_attempts = 12
auth_block_secs = 900
```

查看服务端状态：

```bash
sudo systemctl status nodelite-server.service
sudo journalctl -u nodelite-server.service -f
```

如果服务端起不来，第一时间检查：

```bash
sudo journalctl -u nodelite-server.service -n 100 --no-pager
```

## 认证与安全

NodeLite 的默认安全模型是：面板默认以只读查看为主，Agent 使用逐节点 token 接入，敏感配置优先通过服务端文件、CLI 与受保护的设置页修改。

### Web 面板认证

- `/`、`/nodes/*`、`/api/*` 受只读 Basic Auth 保护。
- 如果 `server.listen` 不是回环地址，配置文件必须提供 `[auth] username/password`，否则服务端会拒绝启动。
- `READONLY_PASSWORD` 或配置文件里的 `auth.password` 至少需要 8 个字符；如果没有同时包含字母和数字，服务端会在启动日志中给出弱密码警告。
- 设置页中的敏感操作（修改密码、启停 2FA、手动更新）会再次要求当前密码或 2FA 验证，不依赖单纯的前端按钮显示。
- 前端会记录本浏览器的登录时间，超过 24 小时后跳转到 `/logout-and-reauth`，触发浏览器重新认证。这是浏览器侧的便利逻辑（JS + localStorage），并不是安全边界——攻击者禁用 JS 或篡改 localStorage 都可以绕过它。真正的过期由服务端 Cookie 的 `Max-Age` 与服务端 session store 共同强制：到点后 cookie 被浏览器丢弃，store 内的票据也会被 prune，受保护接口照常返回 401。

### 可选 TOTP 2FA

TOTP 默认关闭。要开启二次验证，在 `server.toml` 里写：

```toml
[auth]
username = "viewer"
password = "a-strong-password-123"
enable_2fa = true
totp_secret = "JBSWY3DPEHPK3PXP"
```

生成一个新的 base32 secret：

```bash
python3 - <<'PY'
import base64, secrets
print(base64.b32encode(secrets.token_bytes(20)).decode().rstrip("="))
PY
```

把 secret 加进认证器 App 时，可以手工录入，也可以按下面格式生成二维码：

```text
otpauth://totp/NodeLite:viewer?secret=<totp_secret>&issuer=NodeLite
```

2FA 行为说明：

- 登录流程是 `Basic Auth -> /verify-2fa 输入 6 位 TOTP -> 进入面板`。
- TOTP 校验允许前后各一个 30 秒窗口的时钟偏差，并以常量时间比较 code，同一个 30 秒步长在 90 秒内不允许被两次使用（RFC 6238 §5.2 重放防御）。
- Basic Auth 通过后的 TOTP 等待窗口是 5 分钟。同一个 pending session 连续 5 次输错 TOTP 即被强制失效，攻击者必须重新走 Basic Auth 才能再次尝试；客户端 IP 还会被 `[ws] auth_fail_*` 阈值限流。
- 2FA 通过后的会话有效期是 24 小时，cookie 为 `HttpOnly`、`SameSite=Strict`；`public_base_url` 必须是 `https://`，cookie 会带 `Secure`——否则启动时会拒绝配置，避免 TOTP 与 cookie 在明文链路上传输。
- 2FA 默认关闭，旧配置不需要修改即可继续运行。

### Agent Token 生命周期

- 每个节点都有独立 token，存放在服务端 `server.json` 中。
- 新签发或轮换的 node token 默认 90 天有效。
- 服务端在 WebSocket `hello` 阶段检查 token 是否过期，过期时会先下发一条 `token expired; run install-agent --rotate-token...` 的 Error notice 再关闭连接，方便运维从 Agent 日志直接看到补救步骤。
- 已认证的长连接会在 token 距离过期不足 7 天时自动刷新；服务端先把刷新帧发出去，确认 send 成功后才把会话内的 token 视为新值，避免"server 内存里是新 token 但 agent 没收到"的不一致状态。Agent 收到后用 fsync + 0o600 原子写回 `agent.toml`，crash 不会留下空文件。
- 旧版 `server.json` 中没有过期时间的 token 会在节点下一次在线会话里被自动刷新成 90 天 token。
- 如果某台 Agent 离线超过 token 有效期，它无法再用旧 token 自动刷新；Agent 进入 1 小时间隔的退避，等运维在服务端执行 `install-agent --rotate-token` 并替换该节点的 `agent.toml` 后才能恢复。

## Nginx 反代示例

如果你用 Nginx，可以参考：

```nginx
server {
    listen 80;
    server_name monitor.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name monitor.example.com;

    ssl_certificate     /path/to/fullchain.pem;
    ssl_certificate_key /path/to/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    location /ws {
        proxy_pass http://127.0.0.1:8080/ws;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 120s;
        proxy_send_timeout 120s;
    }

    location /install/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

## 节点签发

推荐直接在服务端执行 `install-agent` 子命令。它不会启动监听，只会修改 `server.json`、生成一次性 install token，然后打印一条可直接粘贴到子机上的安装命令。

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  install-agent \
  --node-id hk-01 \
  --node-label "Hong Kong 01" \
  --tag apac \
  --tag edge
```

这个命令会：

- 在 `server.json` 里创建或复用 `hk-01`
- 为该节点生成独立 token
- 生成一个 15 分钟有效的一次性 install token
- 直接打印一条完整的子机安装命令
- 让运行中的服务端在下一次注册表轮询时自动接纳新 token，无需重启进程

注意：

- `/`、`/nodes/*`、`/api/*` 默认受 HTTP Basic Auth 保护
- 安装脚本本身是公开静态文件；真正的节点配置通过一次性 install token 从 `/install/bootstrap` 拉取
- `install-agent` 只会把一次性 install token 内联到命令里，长期 node token 仍然只通过 bootstrap 响应体下发

如果你想看更详细的输出，包括 `agent.toml` 片段和到期时间，也可以继续使用 `issue-node`：

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  issue-node \
  --node-id hk-01 \
  --node-label "Hong Kong 01"
```

如果你需要轮换某个节点 token，可以追加 `--rotate-token`。

如果你只想给已经安装过的 agent 打印一条升级命令，不需要节点参数，可以直接在服务端执行：

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  upgrade-agent
```

## 一键安装

服务端 `install-agent` 打印出来的命令大概长这样：

```bash
curl -fsSL https://monitor.example.com/install/install-agent.sh | \
  NODELITE_AGENT_INSTALL_TOKEN='one-time-token' sh -s -- \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

说明：

- 脚本会检测架构并下载对应的 `nodelite-agent-<target>` 二进制
- 脚本会先把 GitHub `latest` 解析成具体 tag，再下载同一个 release 下的 `SHA256SUMS.txt` 和 agent 二进制，避免刚发版时 CDN 短时间不一致
- 一次性 install token 已经内联在命令里，所以正常情况下不需要再手工输入
- 长期 node token 只通过 bootstrap 响应体下发，不出现在 URL 或命令参数里
- 会创建 `nodelite-agent` 专用系统用户，并以该用户运行 systemd service
- 会写入 `/etc/nodelite/agent.toml`，并将目录/文件权限收紧到仅 root 与该服务用户可读
- 会生成带最小权限沙箱限制的 `nodelite-agent.service`
- 会执行 `daemon-reload`、`enable` 和 `restart`

### 子机安装步骤

推荐按下面顺序操作：

1. 在服务端执行 `install-agent`
2. 复制它打印出的安装命令到目标 Linux 子机
3. 子机直接执行
4. 等脚本结束后检查服务状态

检查 Agent 服务：

```bash
sudo systemctl status nodelite-agent.service
sudo journalctl -u nodelite-agent.service -f
```

如果你想手工运行安装脚本，也可以：

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

如果这台子机已经装过了，后续升级可以更简单，不需要重新拿 bootstrap：

```bash
curl -fsSL https://monitor.example.com/install/install-agent.sh | \
  NODELITE_AGENT_MODE=upgrade sh -s -- \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

升级模式会：

- 只替换 agent 二进制
- 重写并补齐 systemd service
- 保留现有 `/etc/nodelite/agent.toml`
- 自动修正目录和文件权限
- 清理旧版本曾经写入的 `nodelite-agent-auto-update.*` 定时更新单元

如果你在升级时也传了 `--bootstrap-url` 和 install token，它会顺手刷新 agent 配置。

NodeLite 目前不再安装无人值守自动更新 timer。升级建议由管理员手动触发：先确认 release notes 与协议兼容，再执行 `--mode upgrade` 或在已鉴权的面板设置页点击更新。Agent 与 Server 的 WebSocket 握手包含显式 `protocol_version`，如果未来跨大版本出现不兼容协议，Server 会在握手阶段拒绝不匹配的 Agent 并记录警告，而不是让节点以未知协议继续运行。

如果你已经有精确二进制地址，也可以继续使用自定义下载地址和校验文件：

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --checksums-url https://your-host/releases/SHA256SUMS.txt \
  --binary-url https://your-host/releases/nodelite-agent-x86_64-unknown-linux-musl
```

如果 `--binary-url` 使用了自定义文件名，建议直接显式传入对应架构的 SHA-256，避免脚本按 release artifact 名称匹配 `SHA256SUMS.txt` 时找不到条目：

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --binary-url https://your-host/releases/agent-linux-x86_64 \
  --sha256-x86_64 <64-character-sha256>
```

## 手工 Agent 启动

如果你暂时不想用安装脚本，也可以手工部署 agent。

1. 复制配置：

```bash
cp config/agent.example.toml config/agent.toml
```

2. 把 `node_id`、`node_label`、`server`、`token` 替换成服务端签发输出的内容。

3. 本机采样自检：

```bash
cargo run -p nodelite-agent -- --config config/agent.toml --sample-once
```

4. 正常运行：

```bash
cargo run -p nodelite-agent -- --config config/agent.toml
```

## 常见排障

- 面板能打开但没有节点，先看 Agent 日志里是不是 `wss://.../ws` 证书或反代问题。
- 如果服务端日志里频繁出现 TLS 警告，说明你还在用 `http://` 或 `ws://` 明文链路。
- 如果刚关闭 2FA 但服务端因为 `totp_secret` 配置起不来，请升级到 `1.2.16` 或更新版本。
- 如果子机安装时提示 `invalid install token`，通常是一次性 token 过期了，重新执行一次 `install-agent` 或 `issue-node` 即可。
- 如果 Agent 被 `/ws` 限流挡住，先检查服务端 `[ws]` 配额是否太小，或者反代是否把所有请求都转成同一个源 IP。

## GitHub Release

仓库内置了一个 tag 驱动的发布工作流。当推送新的语义化版本 tag，例如 `1.0.0` 或 `v1.0.0` 时，GitHub Actions 会自动：

1. 交叉编译 Linux `x86_64-unknown-linux-musl`
2. 交叉编译 Linux `aarch64-unknown-linux-musl`
3. 生成 `nodelite-server-x86_64-unknown-linux-musl`
4. 生成 `nodelite-agent-x86_64-unknown-linux-musl`
5. 生成 `nodelite-server-aarch64-unknown-linux-musl`
6. 生成 `nodelite-agent-aarch64-unknown-linux-musl`
7. 上传 `install-server.sh` 和 `install-agent.sh`
8. 上传 `SHA256SUMS.txt`
9. 自动创建 GitHub Release

GitHub Release 编译出来的 agent 会把对应 tag 版本号上报到面板里，所以面板里看到的 Agent 版本会是 `1.0.x` 这种发布版本，而不是固定的开发版本号。

## 说明

- 面板主体以只读监控为主；设置页提供有限的受保护写操作，例如密码修改、2FA 开关和手动更新。
- `/healthz` 和 `/ws` 不走只读面板鉴权；面板和 JSON API 走 HTTP Basic Auth；安装脚本和 bootstrap 接口使用独立安装流程。
- agent 只接受服务端 `server.json` 中已登记节点的逐节点 token。
- 首版 agent 只支持 Linux。
- 当前历史图保存基础趋势，不做长期归档。
- 生产环境建议放在 Nginx 或 Caddy 后面并启用 HTTPS。
