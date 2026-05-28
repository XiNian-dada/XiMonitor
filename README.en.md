![NodeLite Banner](images/en/banner.png)

[**中文**](README.md) | [**English**](README.en.md)

[![CI](https://github.com/XiNian-dada/NodeLite/actions/workflows/ci.yml/badge.svg)](https://github.com/XiNian-dada/NodeLite/actions/workflows/ci.yml)
[![Coverage](https://github.com/XiNian-dada/NodeLite/actions/workflows/coverage.yml/badge.svg)](https://github.com/XiNian-dada/NodeLite/actions/workflows/coverage.yml)

# NodeLite

NodeLite is a lightweight server monitoring dashboard written in Rust, including:

- `nodelite-server`
  Central service that provides WebSocket ingress, read-only pages, read-only JSON API, SQLite short-term history, and snapshot recovery.
- `nodelite-agent`
  Linux / macOS agent that collects CPU, load, memory, disk, network total traffic, real-time rates, and WebSocket RTT.
- `nodelite-proto`
  Shared configuration, protocol, and data models for both server and agent.

It fits scenarios where:

- You want a monitoring system like "server + one-command agent install"
- You want the dashboard itself to be as lightweight as possible, prioritizing stability, low footprint, and easy deployment
- You prefer the web UI to be read-only, with sensitive configuration handled through server-side files and controlled CLI entry points

## Quick Start

Full deployment documentation is available on GitHub Pages:
[https://xinian-dada.github.io/NodeLite/](https://xinian-dada.github.io/NodeLite/)

Follow these 3 steps for a complete walkthrough:

1. Install the server

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | sudo sh
```

2. Issue an agent install command from the server

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  install-agent \
  --node-id hk-01 \
  --node-label "Hong Kong 01"
```

3. Paste the printed command on the target Linux machine

After installation:

- Dashboard is accessible via `https://your-domain/`
- Agent connects automatically via `wss://your-domain/ws`
- Historical data is retained for 14 days by default

## Supported Platforms

- `nodelite-server`
  Recommended for Linux (systemd environment). Official builds provide `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`
- `nodelite-agent`
  Supports Linux and macOS; official binaries cover Linux and macOS (Intel / Apple Silicon). macOS one-click install / launchd integration is still experimental — verify on a test machine before long-term use
- Reverse proxy
  Nginx or Caddy recommended for HTTPS / WSS termination

## Screenshots

### Home
![Home](images/en/home_page.png)

### System Settings
![System Settings](images/en/syssettings_page.png)

### Node Details
![Node Details](images/en/detail_page.png)

### Real-time Monitor
![Real-time Monitor](images/en/monitor_page.png)

### Hardware Info
![Hardware Info](images/en/hardware_page.png)

### Network Monitor
![Network Monitor](images/en/network_page.png)

### Agent Logs
![Agent Logs](images/en/agentlog_page.png)

### Agent Settings
![Agent Settings](images/en/agentsettings_page.png)

## Performance

### Runtime Resource Usage Observations

Under a production environment with 4 monitored nodes, long-term observations for `v2.1.2` are approximately:

- **Server memory usage**: 4-10 MB
- **Agent cold-start memory**: ~800 KB
- **Agent after 24 hours**: ~1.2 MB
- **Agent after 72 hours**: ~3 MB

These are observations, not hard limits. The current Agent resident memory increases slowly over time, so `800-1000 KB` is better understood as a cold-start baseline rather than steady-state usage after extended running.

If you are sensitive to Agent long-term resident memory, monitor RSS changes in your own environment and leave headroom.

### v2.2.6 Release Benchmark Baseline

The following data was obtained for `v2.2.6` by running the built-in loopback benchmark `3` times on the same machine and averaging the results, using a release build:

```bash
cargo test -p nodelite-server --release load_test_scaling_scores -- --ignored --nocapture
cargo test -p nodelite-server --release load_test_api_surface_scores -- --ignored --nocapture
cargo test -p nodelite-server --release load_test_reconnect_storm_scores -- --ignored --nocapture
```

Test machine: `Apple M1 Pro / 32 GB / macOS 26.5`

This baseline uses real WebSocket connections, real `metrics` reporting, and real read-only API polling, but is still single-machine loopback — it does not include reverse proxy, TLS termination, or cross-machine network jitter.

Compared with the previous `v2.2.5` baseline recorded in the README, the current sample shows:

- In the `1000`-node dashboard fanout scenario, `/api/nodes` response body dropped from `968995 B` to `285001 B`, about `70.6%` smaller.
- In the `1000`-node dashboard fanout scenario, `/api/nodes` p95 dropped from `12.61 ms` to `3.51 ms`, about `72.2%` lower.
- In the `1000`-node dashboard fanout scenario, `/metrics` p95 dropped from `20.22 ms` to `7.29 ms`, about `64.0%` lower.
- In the `1000`-node history pressure scenario, `history` p95 dropped from `70.90 ms` to `33.86 ms`, about `52.2%` lower.
- In the `500`-node `64`-disk-entry scenario, `/metrics` response body dropped from `3146308 B` to `637182 B`, about `79.7%` smaller.

#### Throughput & Overview Latency

| Nodes | Connection Time | Settle Time | Total Metrics | Metrics Throughput | overview p50 | overview p95 | overview max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 20 | 186.5 ms | 22.3 ms | 240 | 10761.2/s | 0.45 ms | 0.62 ms | 0.83 ms |
| 50 | 456.6 ms | 21.7 ms | 600 | 27671.0/s | 0.46 ms | 0.87 ms | 2.72 ms |
| 100 | 1131.8 ms | 22.5 ms | 1200 | 53484.1/s | 0.58 ms | 1.42 ms | 5.01 ms |
| 200 | 2269.4 ms | 36.5 ms | 2400 | 72864.0/s | 0.54 ms | 5.30 ms | 12.55 ms |

#### 200-Node Read API Latency

`load_test_api_surface_scores` continuously reports `3600` metrics under steady-state while probing read-only APIs for 20 rounds; the history endpoint returns a node's `360` seeded history points within the exact time range. The table below is also the average of `3` repeated runs.

| Endpoint | p50 | p95 | max |
| --- | ---: | ---: | ---: |
| `/api/overview` | 0.66 ms | 2.72 ms | 6.54 ms |
| `/api/nodes` | 0.67 ms | 2.93 ms | 3.48 ms |
| `/api/nodes/{node_id}` | 0.66 ms | 2.98 ms | 3.21 ms |
| `/api/nodes/{node_id}/history` | 1.59 ms | 3.54 ms | 4.99 ms |

#### 200-Node Reconnect Storm

`load_test_reconnect_storm_scores` connects and disconnects `200` nodes repeatedly for `4` cycles, totaling `800` session establishments. The values below are also averaged over `3` repeated runs:

- **Batch connect p95**: 1822.13 ms
- **Last-cycle metric recovery p95**: 74.66 ms
- **Batch disconnect p95**: 22.84 ms
- **Storm `/api/overview` p95**: 2.56 ms
- **Storm `/api/nodes` p95**: 3.12 ms

Notes:

- "Connection time" refers to the time to batch-establish WebSocket connections and complete authentication.
- "Settle time" refers to the time for the final metrics report to arrive and the server state to update.
- These results primarily demonstrate rough magnitude for the current version and are not equivalent to production SLAs.
- Actual performance is affected by build profile, reverse proxy, SQLite I/O, history retention, TLS, and host network conditions.

#### Large-Scale Regression Benchmarks

The following benchmarks are marked `ignored` by default and only run when manually checking for regressions; they do not affect CI:

```bash
cargo test -p nodelite-server --release load_test_large_fleet_scores -- --ignored --nocapture
cargo test -p nodelite-server --release load_test_dashboard_fanout_scores -- --ignored --nocapture
cargo test -p nodelite-server --release load_test_history_pressure_scores -- --ignored --nocapture
cargo test -p nodelite-server --release load_test_payload_size_scores -- --ignored --nocapture
```

Coverage:

- `load_test_large_fleet_scores`: `500` / `1000` nodes loopback WebSocket reporting, sampling `/api/overview`, `/api/nodes`, and Prometheus `/metrics` simultaneously.
- `load_test_dashboard_fanout_scores`: `1000` nodes, `20` concurrent dashboard readers refreshing, interleaved with Prometheus scrapes.
- `load_test_history_pressure_scores`: `1000` nodes with concurrent history query pressure, covering SQLite read-write contention.
- `load_test_payload_size_scores`: `500` nodes with `64` disk entries per node, observing API body size and rendering overhead under large payloads.

Each `*_RESULT` output includes p95 latency, API body bytes as `p50/p95/max`, current process RSS, history writer queue depth, dropped writes, and SQLite `db/wal/shm` file sizes.

The table below is a single-run sample on the same machine, showing approximate magnitude for larger scenarios — not a multi-run averaged baseline:

| Scenario | Key Scale | Metrics Throughput | Key p95 | Response Body Size | RSS | History Dropped Writes |
| --- | --- | ---: | --- | --- | ---: | ---: |
| `load_test_large_fleet_scores` | `500` nodes | `68678.5/s` | `overview 1.18 ms` / `nodes 1.67 ms` / `metrics 7.55 ms` | `overview 257 B` / `nodes 142501 B` / `metrics 637186 B` | `194.8 MiB` | `0` |
| `load_test_large_fleet_scores` | `1000` nodes | `89675.3/s` | `overview 1.56 ms` / `nodes 4.08 ms` / `metrics 18.54 ms` | `overview 261 B` / `nodes 285001 B` / `metrics 1264700 B` | `335.7 MiB` | `0` |
| `load_test_dashboard_fanout_scores` | `1000` nodes + `20` readers | `88934.8/s` | `overview 2.08 ms` / `nodes 3.51 ms` / `metrics 7.29 ms` | `overview 261 B` / `nodes 285001 B` / `metrics 1264700 B` | `337.9 MiB` | `0` |
| `load_test_history_pressure_scores` | `1000` nodes + `20` history readers | `90773.1/s` | `history 33.86 ms` | `history 50823 B` | `334.6 MiB` | `0` |
| `load_test_payload_size_scores` | `500` nodes + `64` disk entries | `43559.1/s` | `nodes 1.55 ms` / `metrics 14.81 ms` | `nodes 142501 B` / `metrics 637182 B` | `222.8 MiB` | `0` |

Homepage DOM rendering pressure can be tested using the real `nodelite-server/assets/index.html` to generate self-contained fixtures:

```bash
node scripts/benchmark-index-dom.mjs --nodes 500
node scripts/benchmark-index-dom.mjs --nodes 1000
```

The script writes to `target/load-test/index-dom-*.html`. After opening the generated file in a browser, the bottom-right corner shows `renderMs`, `jsHeapBytes`, `domNodeCount`, and `nodeCardCount`; the same results are also attached to `window.__NODELITE_DOM_BENCHMARK__` for console access. To simulate large disk payloads, append `--disks 64`.

## Current Capabilities

- One-click install and upgrade:
  - `install-server.sh`
  - `install-agent.sh`
  - `nodelite-server install-agent`
  - `nodelite-server upgrade-agent`
- Server read-only pages:
  - `/`
  - `/nodes/{node_id}`
- Server read-only API:
  - `/api/overview`
  - `/api/nodes`
  - `/api/nodes/{node_id}`
  - `/api/nodes/{node_id}/history`
- Agent connection protocol:
  - `hello`
  - `metrics`
  - `ping`
  - `pong`
  - `server_notice`
  - `refresh_token_request`
  - `refresh_token_response`
- Settings page:
  - View server version, paths, history retention, and node token expiry
  - Change read-only panel password
  - Enable / disable TOTP 2FA
  - Manually trigger server update and view update logs
- 14-day SQLite history retention
- Snapshot persistence and recovery after process restart
- Agent exponential backoff auto-reconnect

## History Data Semantics

History charts show basic trends, not a complete archive of every `metrics` report. By default, the server writes at most one history point per node every 30 seconds, and returns sampled results suitable for graphing based on the time window and `max_points` in queries.

History writes go through a bounded queue + batch SQLite writer. When the queue is full, the server prioritizes real-time WebSocket heartbeats and dashboard current state, dropping the history write and incrementing `nodelite_history_dropped_writes_total` in `/metrics`. This value should be 0 long-term; if it keeps growing, the history writer cannot keep up with the report rate or disk I/O, and history charts may have gaps, though real-time views continue to update.

## Local Build

```bash
cargo check
```

## Prometheus Scraping

NodeLite provides a protected `/metrics` endpoint outputting Prometheus exposition text. It shares the read-only authentication with the dashboard, so scrapers must use the same Basic Auth credentials.

Verify with `curl` first:

```bash
curl -u viewer:secret https://monitor.example.com/metrics
```

Prometheus example:

```yaml
scrape_configs:
  - job_name: nodelite
    scheme: https
    metrics_path: /metrics
    basic_auth:
      username: viewer
      password: secret
    static_configs:
      - targets:
          - monitor.example.com
```

Common operational metrics:

- `nodelite_history_dropped_writes_total`: Total history points dropped when the history write queue is full.
- `nodelite_history_queue_depth` / `nodelite_history_queue_capacity`: Current queue depth and capacity of the history writer.
- `nodelite_audit_dropped_writes_total`: Total audit events dropped when the audit write queue is full.
- `nodelite_audit_queue_depth` / `nodelite_audit_queue_capacity`: Current queue depth and capacity of the audit writer.
- `nodelite_audit_write_failures_total`: Total audit writer enqueue or write failures.
- `nodelite_view_cache_hits_total{kind}` / `nodelite_view_cache_misses_total{kind}`: Cache hits and misses for `overview`, `nodes`, and `metrics` response bodies.
- `nodelite_api_body_bytes{kind}` / `nodelite_metrics_response_body_bytes`: Most recently built API, base `/metrics`, and final `/metrics` response body sizes.
- `nodelite_process_resident_memory_bytes`: Server process RSS, for tracking resident memory growth.
- `nodelite_sqlite_file_bytes{kind}`: history/audit SQLite main file, WAL, and SHM file sizes.
- `nodelite_sqlite_wal_checkpoint_observed{database}`: Whether the most recent passive WAL checkpoint probe succeeded.
- `nodelite_sqlite_wal_checkpoint_active{database}`: Whether the SQLite database is currently in WAL journal mode.
- `nodelite_sqlite_wal_checkpoint_busy{database}`: Busy flag returned by `PRAGMA wal_checkpoint(PASSIVE)`.
- `nodelite_sqlite_wal_checkpoint_pages{database,state}`: `log`, `checkpointed`, and `backlog` WAL page counts. When `backlog` grows persistently alongside WAL file bytes, it usually indicates checkpoint is being held back by long-running read transactions or disk I/O.
- `nodelite_registry_nodes`: Number of currently loaded registered nodes.
- `nodelite_registry_disk_entries_total`: Total disk entries held across all node snapshots.
- `nodelite_ws_messages_total{type}`: Cumulative count of authenticated WebSocket messages by type.

SQLite operational tips:

- History uses WAL mode for better write/query concurrency. `/metrics` performs a controlled `wal_checkpoint(PASSIVE)` probe at most every 60 seconds. `PASSIVE` does not truncate WAL or wait for other connections to release locks; it attempts to advance checkpointable WAL pages and returns the current WAL page count, checkpointed pages, and busy status.
- History and Audit retention pruning uses `DELETE` to remove expired rows. The SQLite main database file does not shrink immediately from `DELETE`, and WAL may remain large before checkpoint — this is normal SQLite file reuse behavior.
- When `nodelite_sqlite_file_bytes{kind="history_wal"}` or `kind="audit_wal"` grows persistently, alert on `nodelite_sqlite_wal_checkpoint_pages{state="backlog"}` and the busy flag simultaneously to distinguish between write growth, blocked checkpoint, and external disk issues.
- To reclaim database file bloat, stop the service or ensure no long-running transactions exist, then manually run `VACUUM` during a maintenance window. To force WAL truncation, run `PRAGMA wal_checkpoint(TRUNCATE)` on the target database during a maintenance window. Do not put these operations on the automatic hot path during peak hours.

## Test Coverage

Install [cargo-tarpaulin](https://github.com/xd009642/tarpaulin) (one-time, Linux only):

```bash
cargo install cargo-tarpaulin
```

Run coverage analysis:

```bash
cargo tarpaulin --config tarpaulin.toml
```

HTML report is written to `target/tarpaulin/tarpaulin-report.html`, viewable in a browser for line-by-line coverage.

### Coverage Targets

| Phase | Target | Timeline |
| --- | --- | --- |
| Baseline | Record current value | Immediate |
| Short-term | 75% | Within 2 weeks |
| Long-term | 80% | Ongoing |

Priority coverage modules: auth, admission, sanitize, registry (security-critical paths).

To run specific property tests independently:

```bash
cargo test -p nodelite-server sanitize::tests
cargo test -p nodelite-server registry::tests
```

## Cross-Compilation Linux x86_64 / aarch64

The repository includes `lld` linker configuration for musl targets, enabling static Linux binaries:

```bash
cargo build --release --target x86_64-unknown-linux-musl \
  -p nodelite-server \
  -p nodelite-agent

cargo build --release --target aarch64-unknown-linux-musl \
  -p nodelite-server \
  -p nodelite-agent
```

Output locations:

```bash
target/x86_64-unknown-linux-musl/release/nodelite-server
target/x86_64-unknown-linux-musl/release/nodelite-agent
target/aarch64-unknown-linux-musl/release/nodelite-server
target/aarch64-unknown-linux-musl/release/nodelite-agent
```

## Recommended Deployment Topology

For production, set it up like this:

1. `nodelite-server` listens on `127.0.0.1:8080`
2. Nginx or Caddy exposes `443` externally
3. Dashboard and API go through HTTPS
4. Agent connects via `wss://your-domain/ws`

This keeps TLS, access logs, rate limiting, and basic access control in the reverse proxy layer.

## Server Deployment

The interactive installer from GitHub Releases is recommended. It clears the screen, asks for installation directory, listen port, public domain or IP, and read-only dashboard credentials, then automatically:

- Downloads the latest `nodelite-server` for the detected architecture
- Fetches `SHA256SUMS.txt` and verifies the binary
- Generates `server.toml` and `server.json`
- Registers and starts `nodelite-server.service`

One-command install:

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | sudo sh
```

The same command can also be used for upgrades later. The script auto-detects existing installations and defaults to `upgrade` mode; you can also force it:

```bash
curl -fsSL https://github.com/XiNian-dada/NodeLite/releases/latest/download/install-server.sh | \
  sudo NODELITE_SERVER_MODE=upgrade sh
```

The script by default:

- Puts program data in the directory you specify (default `/opt/nodelite`)
- Listens on `127.0.0.1:<random port>`
- Asks for the public domain or IP and generates `public_base_url`
- Generates read-only panel Basic Auth credentials
- Creates a systemd service with `NoNewPrivileges`, `ProtectSystem`, `ProtectKernel*`, `CapabilityBoundingSet=` restrictions
- If upgrading, only replaces the binary and systemd unit, preserving existing `server.toml`, dashboard credentials, node tokens, and registry content

After installation, it prints:

- Server binary path
- Config file path
- Node registry path
- Dashboard read-only username and password
- The next `install-agent` command to run

If you prefer manual deployment:

1. Copy [config/server.example.toml](config/server.example.toml) and [config/server.json.example](config/server.json.example)
2. Install the server binary to `/usr/local/bin/nodelite-server`
3. Create a systemd unit manually
4. Start `nodelite-server.service`

Minimum configuration items:

```toml
[server]
listen = "127.0.0.1:28080"
public_base_url = "https://monitor.example.com"
trusted_proxies = ["203.0.113.0/24"]
node_registry_path = "/opt/nodelite/config/server.json"
history_db_path = "/opt/nodelite/data/history.sqlite3"
snapshot_path = "/opt/nodelite/data/snapshot.json"

[auth]
username = "viewer"
password = "change-this-password"
# Disabled by default; must configure totp_secret when enabled.
enable_2fa = false
# totp_secret = "JBSWY3DPEHPK3PXP"

[audit]
enabled = true
db_path = "/opt/nodelite/data/audit.sqlite3"
retention_days = 90
log_successful_auth = true
log_failed_auth = true
log_token_events = true
log_rate_limit = true

[alerts]
enabled = false

[alerts.smtp]
enabled = false
host = ""
port = 587
username = ""
sender = ""
recipients = []
transport = "start_tls"
send_resolved = true

[alerts.webhook]
enabled = false
url = ""
send_resolved = true

[alerts.inspection]
enabled = false
local_time = "09:00"
lookback_hours = 24
delivery = ["smtp"]
offline_grace_minutes = 10
latency_warn_ms = 250
cpu_warn_percent = 85
memory_warn_percent = 90

[ws]
max_total_connections = 1024
max_connections_per_ip = 32
auth_fail_window_secs = 300
auth_fail_max_attempts = 12
auth_block_secs = 900
```

Check server status:

```bash
sudo systemctl status nodelite-server.service
sudo journalctl -u nodelite-server.service -f
```

If the service won't start, first check:

```bash
sudo journalctl -u nodelite-server.service -n 100 --no-pager
```

## Authentication & Security

NodeLite's default security model: the dashboard is read-only by default, agents connect with per-node tokens, and sensitive configuration is primarily handled through server-side files, CLI, and the protected settings page.

### Web Panel Authentication

- `/`, `/nodes/*`, `/api/*` are protected by read-only Basic Auth.
- If `server.listen` is not a loopback address, the config file must provide `[auth] username/password`, or the server will refuse to start.
- `READONLY_PASSWORD` or `auth.password` in config must be at least 8 characters; if it does not contain both letters and digits, the server logs a weak password warning at startup.
- Sensitive operations on the settings page (password change, 2FA toggle, manual update) require the current password or 2FA verification again, not just frontend button visibility.
- The frontend records login time in the browser; after 24 hours it redirects to `/logout-and-reauth`, triggering browser re-authentication. This is a browser-side convenience (JS + localStorage), not a security boundary — attackers disabling JS or tampering with localStorage can bypass it. True expiry is enforced jointly by the server-side cookie `Max-Age` and the server-side session store: the cookie is discarded by the browser at expiry, and store tickets are pruned, so protected endpoints still return 401.

### Optional TOTP 2FA

TOTP is disabled by default. To enable two-factor authentication, set in `server.toml`:

```toml
[auth]
username = "viewer"
password = "a-strong-password-123"
enable_2fa = true
totp_secret = "JBSWY3DPEHPK3PXP"
```

Generate a new base32 secret:

```bash
python3 - <<'PY'
import base64, secrets
print(base64.b32encode(secrets.token_bytes(20)).decode().rstrip("="))
PY
```

When adding the secret to your authenticator app, enter it manually or generate a QR code with the following format:

```text
otpauth://totp/NodeLite:viewer?secret=<totp_secret>&issuer=NodeLite
```

2FA behavior notes:

- Login flow: `Basic Auth -> /verify-2fa enter 6-digit TOTP -> enter dashboard`.
- TOTP validation allows ±1 30-second window clock drift, compares codes in constant time, and the same 30-second step cannot be used twice within 90 seconds (RFC 6238 §5.2 replay protection).
- The TOTP wait window after Basic Auth is 5 minutes. 5 consecutive incorrect TOTP codes on the same pending session invalidate it — the attacker must redo Basic Auth to try again; the client IP is also rate-limited by `[ws] auth_fail_*` thresholds.
- Session validity after 2FA is 24 hours; cookies are `HttpOnly`, `SameSite=Strict`; `public_base_url` must be `https://` and cookies include `Secure` — otherwise startup is refused to prevent TOTP and cookie transmission over cleartext.
- 2FA is disabled by default, so existing configurations work without changes.

### Agent Token Lifecycle

- Each node has an independent token stored in the server's `server.json`.
- Newly issued or rotated node tokens are valid for 30 days by default.
- The server checks token expiry during WebSocket `hello`; if expired, it sends an Error notice with `token expired; run install-agent --rotate-token...` before closing the connection, so operators can see the remediation step in Agent logs.
- Authenticated long-lived connections auto-refresh when the token is within 7 days of expiry; the server sends the refresh frame first, and only considers the session token updated after confirming the send succeeded — avoiding the inconsistency of "server memory has the new token but the agent didn't receive it." The agent writes atomically with fsync + 0o600 to `agent.toml`, so a crash won't leave an empty file.
- Tokens in the old `server.json` without expiry dates are automatically refreshed to 30-day tokens on the node's next online session.
- If an agent stays offline past its token's validity period, the old token cannot be used for auto-refresh; the agent enters a 1-hour-interval backoff, waiting for the operator to run `install-agent --rotate-token` and replace the node's `agent.toml`.

### Audit Logs

- The server writes authentication failures, TOTP verification results, invalid install/node tokens, rate limit blocks, and successful node handshakes to a separate `audit.sqlite3`.
- The `[audit]` section controls whether auditing is enabled, retention days, and which event types are logged; default retention is 90 days.
- `/api/audit-log` uses the same read-only auth as other `/api/*` endpoints, supporting `start`, `end`, `event_type`, `success`, and `limit` query parameters.

For example, view the last 100 failed authentication events:

```text
/api/audit-log?event_type=login_failure&success=false&limit=100
```

## Alerts and Daily Inspection

- The server config now supports an `[alerts]` section for SMTP, WebHook, rule-based alerts, and a daily inspection digest.
- `[[alerts.rules]]` can target `cpu_usage_percent`, `memory_usage_percent`, `disk_usage_percent`, `latency_ms`, and `offline_minutes`, and can scope a rule to all nodes, specific node IDs, or matching tags.
- `[alerts.inspection]` defines the daily digest schedule, lookback window, and inspection thresholds so operators can send a periodic “recent server health” summary.
- This release wires those settings into `server.toml` and the web settings data model first, so alert policy can be managed centrally before validating outbound SMTP / WebHook delivery.

## Nginx Reverse Proxy Example

If you use Nginx, reference this:

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

## Node Registration

Run the `install-agent` subcommand directly on the server. It does not start listening; it only modifies `server.json`, generates a one-time install token, and prints a complete command ready to paste on the target machine.

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  install-agent \
  --node-id hk-01 \
  --node-label "Hong Kong 01" \
  --tag apac \
  --tag edge
```

This command will:

- Create or reuse `hk-01` in `server.json`
- Generate an independent token for this node
- Generate a 15-minute valid one-time install token
- Print a complete target machine install command
- Allow the running server to accept the new token on the next registry poll without restart

Notes:

- `/`, `/nodes/*`, `/api/*` are protected by HTTP Basic Auth by default
- The install script itself is a public static file; actual node configuration is fetched via the one-time install token from `/install/bootstrap`
- `install-agent` only inlines the one-time install token into the command; the long-term node token is only delivered via the bootstrap response body

For more detailed output including `agent.toml` snippet and expiry time, continue using `issue-node`:

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  issue-node \
  --node-id hk-01 \
  --node-label "Hong Kong 01"
```

To rotate a node token, append `--rotate-token`.

To print an upgrade command for an already-installed agent (no node parameters needed), run on the server:

```bash
/usr/local/bin/nodelite-server \
  --config /opt/nodelite/config/server.toml \
  upgrade-agent
```

## One-Click Agent Install

The command printed by `install-agent` looks like this:

```bash
curl -fsSL https://monitor.example.com/install/install-agent.sh | \
  NODELITE_AGENT_INSTALL_TOKEN='one-time-token' sh -s -- \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

Notes:

- The script detects the architecture and downloads the matching `nodelite-agent-<target>` binary
- The script first resolves GitHub `latest` to a specific tag, then downloads both `SHA256SUMS.txt` and the agent binary from the same release to avoid short-lived CDN inconsistencies
- The one-time install token is already inlined in the command, so no manual input is normally needed
- The long-term node token is only delivered via the bootstrap response body, not appearing in URLs or command arguments
- Linux:
  Creates a dedicated `nodelite-agent` system user and runs the systemd service as that user; writes to `/etc/nodelite/agent.toml`; generates `nodelite-agent.service` with minimal privilege sandboxing; runs `daemon-reload`, `enable`, and `restart`
- macOS (experimental):
  Uses a minimal root + launchd implementation; does not create a dedicated service user; generates `/Library/LaunchDaemons/com.nodelite.agent.plist` and starts via `launchctl bootstrap/kickstart`; logs land in `/var/log/nodelite-agent.log` and `/var/log/nodelite-agent.err.log`

### Target Machine Install Steps

Recommended order:

1. Run `install-agent` on the server
2. Copy the printed install command to the target machine (Linux fully supported, macOS experimental)
3. Execute it on the target machine
4. Check the service status after the script finishes

Check Agent service status:

```bash
# Linux
sudo systemctl status nodelite-agent.service
sudo journalctl -u nodelite-agent.service -f

# macOS (experimental)
sudo launchctl print system/com.nodelite.agent
sudo tail -f /var/log/nodelite-agent.log /var/log/nodelite-agent.err.log
```

To run the install script manually:

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

If the machine is already installed, subsequent upgrades can be simpler without re-fetching bootstrap:

```bash
curl -fsSL https://monitor.example.com/install/install-agent.sh | \
  NODELITE_AGENT_MODE=upgrade sh -s -- \
  --base-url https://github.com/XiNian-dada/NodeLite/releases/latest/download
```

Upgrade mode will:

- Only replace the agent binary
- Rewrite and fill in the systemd service on Linux; rewrite and reload the launchd plist on macOS
- Preserve existing `/etc/nodelite/agent.toml`
- Auto-fix directory and file permissions
- On Linux, clean up any `nodelite-agent-auto-update.*` timer units from older versions

If you also pass `--bootstrap-url` and an install token during upgrade, it refreshes the agent configuration.

NodeLite no longer installs unattended auto-update timers. Upgrades should be triggered manually by an operator: review the release notes and protocol compatibility, then run `--mode upgrade` or click update in the authenticated panel settings page. The Agent and Server WebSocket handshake includes an explicit `protocol_version`; if a future major version changes the protocol, the server rejects incompatible agents at handshake time and logs a warning, rather than letting nodes run with an unknown protocol.

If you have exact binary addresses, you can also use custom download URLs and checksum files:

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --checksums-url https://your-host/releases/SHA256SUMS.txt \
  --binary-url https://your-host/releases/nodelite-agent-x86_64-unknown-linux-musl
```

If `--binary-url` uses a custom filename, explicitly pass the SHA-256 for the corresponding architecture so the script can find it without matching release artifact names in `SHA256SUMS.txt`:

```bash
sh scripts/install-agent.sh \
  --bootstrap-url https://monitor.example.com/install/bootstrap \
  --install-token <one-time-token> \
  --binary-url https://your-host/releases/agent-linux-x86_64 \
  --sha256-x86_64 <64-character-sha256>
```

## Manual Agent Startup

If you prefer not to use the install script, you can deploy the agent manually. For macOS, if you don't want to use the experimental install script, download `nodelite-agent-x86_64-apple-darwin` / `nodelite-agent-aarch64-apple-darwin` from GitHub Releases and run it this way.

1. Copy the config:

```bash
cp config/agent.example.toml config/agent.toml
```

2. Replace `node_id`, `node_label`, `server`, and `token` with values from the server's `install-agent` output.

3. Local sampling dry-run:

```bash
cargo run -p nodelite-agent -- --config config/agent.toml --sample-once
```

This step now works on both Linux and macOS. On first sample, CPU percentage and network rate show as `0` / `null` due to lack of a previous baseline — this is normal.

4. Run normally:

```bash
cargo run -p nodelite-agent -- --config config/agent.toml
```

## Common Troubleshooting

- Dashboard opens but no nodes appear: first check the Agent logs for `wss://.../ws` certificate or reverse proxy issues.
- If TLS warnings appear frequently in server logs, you are still using `http://` or `ws://` cleartext links.
- If the server won't start after disabling 2FA due to `totp_secret` config, upgrade to `1.2.16` or later.
- If the target machine reports `invalid install token`, the one-time token has expired — re-run `install-agent` or `issue-node`.
- If the Agent is rate-limited by `/ws`, check if the server `[ws]` quota is too small, or whether the reverse proxy/WAF egress subnet is written into `server.trusted_proxies`; co-located Nginx/Caddy do not need additional configuration.

## GitHub Release

The repository includes a tag-driven release workflow. When a new semantic version tag (e.g., `1.0.0` or `v1.0.0`) is pushed, GitHub Actions automatically:

1. Cross-compiles Linux `x86_64-unknown-linux-musl`
2. Cross-compiles Linux `aarch64-unknown-linux-musl`
3. Natively builds Agent `x86_64-apple-darwin`
4. Natively builds Agent `aarch64-apple-darwin`
5. Generates `nodelite-server-x86_64-unknown-linux-musl`
6. Generates `nodelite-agent-x86_64-unknown-linux-musl`
7. Generates `nodelite-server-aarch64-unknown-linux-musl`
8. Generates `nodelite-agent-aarch64-unknown-linux-musl`
9. Generates `nodelite-agent-x86_64-apple-darwin`
10. Generates `nodelite-agent-aarch64-apple-darwin`
11. Uploads `install-server.sh` and `install-agent.sh`
12. Uploads `SHA256SUMS.txt`
13. Automatically creates a GitHub Release

The agent compiled by GitHub Release reports the corresponding tag version to the dashboard, so the Agent version shown in the panel will be a release version like `1.0.x`, not a fixed development version number.

## Notes

- The dashboard is primarily read-only monitoring; the settings page provides limited protected write operations, such as password changes, 2FA toggle, and manual updates.
- `/healthz` and `/ws` do not use read-only panel auth; dashboard and JSON API use HTTP Basic Auth; install scripts and bootstrap interfaces use a separate install flow.
- Agents only accept per-node tokens from nodes registered in the server's `server.json`.
- Server-internal public boundaries prefer typed errors (e.g., `RegistryError`, `HistoryError`, `AuthSessionError`, `ProtocolError`); `anyhow` is primarily used in module-internal helpers for context enrichment.
- The initial agent only supports Linux.
- History charts save basic trends, not long-term archives; under high pressure, history points may be dropped if the queue is full — see "History Data Semantics" for details.
- Production deployment is recommended behind Nginx or Caddy with HTTPS enabled.
