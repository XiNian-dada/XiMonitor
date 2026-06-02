#!/bin/sh
# NodeLite Agent 一键安装 / 升级脚本。
#
# 主要流程:
#   1. 解析命令行参数与环境变量,确认目标架构与下载源。
#   2. 创建专用服务用户、目录,并按需调用引导接口拉取 agent.toml。
#   3. 校验二进制 SHA-256,落盘到 /usr/local/bin,并写入 systemd unit /
#      launchd plist。
#   4. 启动 / 重启 agent 服务。
#
# 该脚本设计为可被 `curl ... | sh` 直接执行,所以全部用 POSIX shell 实现,
# 不依赖 bash 特性。所有失败都通过 `fail` 输出统一前缀并以非零状态退出。

set -eu
# 默认 umask:确保新建的临时文件不会泄漏给同主机其它用户。
umask 077

# 统一的错误输出函数,前缀方便日志检索。
fail() {
  printf '%s\n' "install-agent: $*" >&2
  exit 1
}

# 检查依赖命令是否存在,缺失则直接退出。
need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

# 对参数进行 POSIX shell 安全的单引号转义,适合嵌入到 systemd 单元等场景。
shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\"'\"'/g")"
}

xml_escape() {
  printf '%s' "$1" | sed \
    -e 's/&/\&amp;/g' \
    -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' \
    -e 's/"/\&quot;/g' \
    -e "s/'/\&apos;/g"
}

# ---- 命令行 / 环境变量入参 ----
BOOTSTRAP_URL=""
INSTALL_TOKEN="${NODELITE_AGENT_INSTALL_TOKEN:-}"
INSTALL_TOKEN_FILE="${NODELITE_AGENT_INSTALL_TOKEN_FILE:-}"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/nodelite"
MODE="${NODELITE_AGENT_MODE:-auto}"
# VERSION 环境变量:默认只安装正式版本。如需 alpha/beta/RC 等预发布版本,
# 必须显式设置 VERSION=v2.3.0-alpha.1,否则安装脚本会拒绝。
# 留空时自动使用 GitHub 最新正式版。
VERSION="${NODELITE_AGENT_VERSION:-}"
if [ -n "$VERSION" ]; then
  BASE_URL="${NODELITE_AGENT_BASE_URL:-https://github.com/XiNian-dada/NodeLite/releases/download/${VERSION}}"
else
  BASE_URL="${NODELITE_AGENT_BASE_URL:-https://github.com/XiNian-dada/NodeLite/releases/latest/download}"
fi
CHECKSUMS_URL="${NODELITE_AGENT_CHECKSUMS_URL:-}"
BINARY_URL="${NODELITE_AGENT_BINARY_URL:-}"
SHA256_X86_64="${NODELITE_AGENT_SHA256_X86_64:-}"
SHA256_AARCH64="${NODELITE_AGENT_SHA256_AARCH64:-}"
SERVICE_USER="nodelite-agent"
SERVICE_GROUP="nodelite-agent"
STATE_DIR="/var/lib/nodelite-agent"
BIN_PATH=""
CONFIG_PATH=""
OS_NAME=""
SERVICE_KIND=""
UNIT_PATH="/etc/systemd/system/nodelite-agent.service"
LAUNCHD_LABEL="com.nodelite.agent"
PLIST_PATH="/Library/LaunchDaemons/com.nodelite.agent.plist"
LAUNCHD_STDOUT_PATH="/var/log/nodelite-agent.log"
LAUNCHD_STDERR_PATH="/var/log/nodelite-agent.err.log"
SERVICE_DEFINITION_PATH=""
SERVICE_STATUS_NAME=""
LEGACY_AUTO_UPDATE_HELPER_PATH="/usr/local/bin/nodelite-agent-auto-update"
LEGACY_AUTO_UPDATE_SERVICE_PATH="/etc/systemd/system/nodelite-agent-auto-update.service"
LEGACY_AUTO_UPDATE_TIMER_PATH="/etc/systemd/system/nodelite-agent-auto-update.timer"
TMP_PATH=""
BOOTSTRAP_TMP=""
CURL_AUTH_CONFIG=""
CHECKSUMS_TMP=""

# 退出时清理临时文件,确保不会残留含 token 的内容。
cleanup() {
  [ -n "$TMP_PATH" ] && rm -f "$TMP_PATH"
  [ -n "$BOOTSTRAP_TMP" ] && rm -f "$BOOTSTRAP_TMP"
  [ -n "$CURL_AUTH_CONFIG" ] && rm -f "$CURL_AUTH_CONFIG"
  [ -n "$CHECKSUMS_TMP" ] && rm -f "$CHECKSUMS_TMP"
}

trap cleanup EXIT HUP INT TERM

# 计算指定文件的 SHA-256 摘要;优先用 GNU `sha256sum`,缺失时回退到 `shasum -a 256`。
calculate_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | sed 's/[[:space:]].*$//'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | sed 's/[[:space:]].*$//'
    return 0
  fi
  fail "missing required command: sha256sum or shasum"
}

# 解析 nologin / false 之类的"禁用登录 shell",供创建服务用户时使用。
resolve_nologin_shell() {
  if command -v nologin >/dev/null 2>&1; then
    command -v nologin
    return 0
  fi
  if [ -x /usr/sbin/nologin ]; then
    printf '%s\n' /usr/sbin/nologin
    return 0
  fi
  if [ -x /sbin/nologin ]; then
    printf '%s\n' /sbin/nologin
    return 0
  fi
  if [ -x /usr/bin/false ]; then
    printf '%s\n' /usr/bin/false
    return 0
  fi
  if [ -x /bin/false ]; then
    printf '%s\n' /bin/false
    return 0
  fi
  fail "unable to find a nologin shell for the service user"
}

# 确保系统中存在用于运行 Agent 的专用账户;不存在则创建。
ensure_service_account() {
  if id -u "$SERVICE_USER" >/dev/null 2>&1; then
    return 0
  fi

  NOLOGIN_SHELL="$(resolve_nologin_shell)"
  if command -v useradd >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /nonexistent \
      --shell "$NOLOGIN_SHELL" --user-group "$SERVICE_USER" \
      || fail "failed to create service user $SERVICE_USER"
    return 0
  fi
  if command -v adduser >/dev/null 2>&1; then
    adduser --system --group --no-create-home --home /nonexistent \
      --shell "$NOLOGIN_SHELL" "$SERVICE_USER" \
      || fail "failed to create service user $SERVICE_USER"
    return 0
  fi

  fail "missing required command: useradd or adduser"
}

# 通过 /dev/tty 提示用户手动输入一次性安装令牌,输入过程中关闭回显。
prompt_install_token() {
  need_cmd stty
  [ -r /dev/tty ] || fail "missing install token and no interactive terminal is available"

  old_tty_state="$(stty -g </dev/tty)" || fail "failed to inspect terminal state"
  trap 'stty "$old_tty_state" </dev/tty; cleanup' EXIT HUP INT TERM
  printf '%s' "One-time install token: " >/dev/tty
  stty -echo </dev/tty || fail "failed to disable terminal echo"
  IFS= read -r INSTALL_TOKEN </dev/tty || fail "failed to read install token"
  stty "$old_tty_state" </dev/tty || fail "failed to restore terminal state"
  trap cleanup EXIT HUP INT TERM
  printf '\n' >/dev/tty
}

# 按优先级加载安装令牌:文件 > 环境变量 > 交互输入。
load_install_token() {
  if [ -n "$INSTALL_TOKEN_FILE" ]; then
    [ -r "$INSTALL_TOKEN_FILE" ] || fail "install token file is not readable: $INSTALL_TOKEN_FILE"
    INSTALL_TOKEN="$(sed -n '1p' "$INSTALL_TOKEN_FILE")"
  elif [ -z "$INSTALL_TOKEN" ]; then
    prompt_install_token
  fi

  [ -n "$INSTALL_TOKEN" ] || fail "install token must not be empty"
}

# 把 Authorization 头写入 curl 的配置文件,避免它出现在 `ps` 中。
write_curl_auth_config() {
  cat >"$CURL_AUTH_CONFIG" <<EOF
header = "Authorization: Bearer $INSTALL_TOKEN"
EOF
  chmod 0600 "$CURL_AUTH_CONFIG"
}

# 拉取引导接口返回的 agent.toml,并做简单的内容自检。
fetch_bootstrap_config() {
  [ -n "$BOOTSTRAP_URL" ] || fail "missing --bootstrap-url"
  load_install_token
  write_curl_auth_config
  printf '%s\n' "Fetching agent bootstrap from $BOOTSTRAP_URL"
  curl -fsSL --config "$CURL_AUTH_CONFIG" "$BOOTSTRAP_URL" -o "$BOOTSTRAP_TMP" \
    || fail "failed to fetch agent bootstrap config"
  grep -q '^\[agent\]$' "$BOOTSTRAP_TMP" || fail "bootstrap response did not contain an agent config"
  grep -q '^token = "' "$BOOTSTRAP_TMP" || fail "bootstrap response did not contain an agent token"
}

# 解析发布源对应的 SHA256SUMS.txt,挑出当前架构产物的预期摘要。
fetch_expected_sha256() {
  artifact_name="$1"

  if [ -n "$EXPECTED_SHA256" ]; then
    return 0
  fi

  if [ -z "$CHECKSUMS_URL" ]; then
    CHECKSUMS_URL="$BASE_URL/SHA256SUMS.txt"
  fi

  printf '%s\n' "Fetching checksums from $CHECKSUMS_URL"
  curl -fsSL "$CHECKSUMS_URL" -o "$CHECKSUMS_TMP" \
    || fail "failed to fetch release checksums"

  EXPECTED_SHA256="$(awk -v artifact="$artifact_name" '
    NF >= 2 {
      path = $2
      sub(/^\*/, "", path)
      count = split(path, parts, "/")
      if (parts[count] == artifact) {
        print $1
        exit
      }
    }
  ' "$CHECKSUMS_TMP")"

  [ -n "$EXPECTED_SHA256" ] || fail "missing checksum entry for $artifact_name"
}

# 检查版本号是否是正式版（不含 alpha/beta/RC 标记）。
is_stable_version() {
  case "$1" in
    *-alpha*|*-beta*|*-rc*|*-pre*|*-test*)
      return 1
      ;;
  esac
  return 0
}

# 把 `releases/latest/download` 形式的下载源解析成具体 tag,避免每次升级又跳到最新版。
resolve_release_base_url() {
  if [ -n "$VERSION" ]; then
    printf '%s\n' "Installing specified version: $VERSION"
    if ! is_stable_version "$VERSION"; then
      printf '%s\n' "Warning: $VERSION is a pre-release. Use at your own risk." >&2
    fi
    return 0
  fi
  case "$BASE_URL" in
    https://github.com/*/releases/latest/download)
      releases_root="${BASE_URL%/latest/download}"
      redirect_location="$(curl -fsSI "$releases_root/latest" | awk '
        tolower($1) == "location:" {
          value = $2
          sub(/\r$/, "", value)
          location = value
        }
        END {
          if (location != "") {
            print location
          }
        }
      ')" \
        || fail "failed to resolve latest GitHub release"
      [ -n "$redirect_location" ] || fail "GitHub latest release redirect did not include a location"
      resolved_tag="${redirect_location##*/}"
      if ! is_stable_version "$resolved_tag"; then
        fail "resolved latest release '${resolved_tag}' is a pre-release. Set NODELITE_AGENT_VERSION=${resolved_tag} to install it explicitly."
      fi
      BASE_URL="${releases_root}/download/${resolved_tag}"
      printf '%s\n' "Resolved GitHub latest release tag: $resolved_tag"
      ;;
  esac
}

configure_platform() {
  OS_NAME="$(uname -s)"
  case "$OS_NAME" in
    Linux)
      SERVICE_KIND="systemd"
      SERVICE_DEFINITION_PATH="$UNIT_PATH"
      SERVICE_STATUS_NAME="nodelite-agent.service"
      ;;
    Darwin)
      SERVICE_KIND="launchd"
      SERVICE_DEFINITION_PATH="$PLIST_PATH"
      SERVICE_STATUS_NAME="$LAUNCHD_LABEL"
      ;;
    *)
      fail "unsupported operating system: $OS_NAME"
      ;;
  esac
}

prepare_service_account() {
  if [ "$SERVICE_KIND" = "systemd" ]; then
    ensure_service_account
    SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
    return 0
  fi

  SERVICE_USER="root"
  SERVICE_GROUP="$(id -gn root)"
}

prepare_directories() {
  mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$STATE_DIR"
  chown root:root "$INSTALL_DIR"
  chmod 0755 "$INSTALL_DIR"

  if [ "$SERVICE_KIND" = "systemd" ]; then
    chown root:"$SERVICE_GROUP" "$CONFIG_DIR" "$STATE_DIR"
    chmod 0750 "$CONFIG_DIR" "$STATE_DIR"
    return 0
  fi

  chown root:"$SERVICE_GROUP" "$CONFIG_DIR" "$STATE_DIR"
  chmod 0700 "$CONFIG_DIR" "$STATE_DIR"
}

install_agent_config() {
  config_mode="0640"
  if [ "$SERVICE_KIND" = "launchd" ]; then
    config_mode="0600"
  fi

  if [ "$config_refreshed" -eq 1 ]; then
    install -o root -g "$SERVICE_GROUP" -m "$config_mode" "$BOOTSTRAP_TMP" "$CONFIG_PATH"
    return 0
  fi

  chown root:"$SERVICE_GROUP" "$CONFIG_PATH"
  chmod "$config_mode" "$CONFIG_PATH"
}

# 自动更新支持已移除。升级时顺手清理旧版本曾经写入的 timer / service,
# 避免无人值守任务继续从 latest 通道自我替换 agent。
cleanup_legacy_auto_update() {
  if [ "$SERVICE_KIND" != "systemd" ]; then
    return 0
  fi

  systemctl disable --now nodelite-agent-auto-update.timer >/dev/null 2>&1 || true
  systemctl disable nodelite-agent-auto-update.service >/dev/null 2>&1 || true
  rm -f "$LEGACY_AUTO_UPDATE_HELPER_PATH" \
    "$LEGACY_AUTO_UPDATE_SERVICE_PATH" \
    "$LEGACY_AUTO_UPDATE_TIMER_PATH"
}

write_systemd_unit() {
  cat >"$UNIT_PATH" <<EOF
[Unit]
Description=NodeLite Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN_PATH --config $CONFIG_PATH
Restart=always
RestartSec=3
TimeoutStopSec=15s
User=$SERVICE_USER
Group=$SERVICE_GROUP
WorkingDirectory=$STATE_DIR
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=full
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
RestrictSUIDSGID=true
RestrictRealtime=true
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native
CapabilityBoundingSet=
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
SystemCallFilter=@system-service

[Install]
WantedBy=multi-user.target
EOF
}

write_launchd_plist() {
  escaped_bin_path="$(xml_escape "$BIN_PATH")"
  escaped_config_path="$(xml_escape "$CONFIG_PATH")"
  escaped_state_dir="$(xml_escape "$STATE_DIR")"
  escaped_stdout_path="$(xml_escape "$LAUNCHD_STDOUT_PATH")"
  escaped_stderr_path="$(xml_escape "$LAUNCHD_STDERR_PATH")"

  cat >"$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LAUNCHD_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$escaped_bin_path</string>
    <string>--config</string>
    <string>$escaped_config_path</string>
  </array>
  <key>WorkingDirectory</key>
  <string>$escaped_state_dir</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>Umask</key>
  <integer>63</integer>
  <key>StandardOutPath</key>
  <string>$escaped_stdout_path</string>
  <key>StandardErrorPath</key>
  <string>$escaped_stderr_path</string>
</dict>
</plist>
EOF
  chown root:wheel "$PLIST_PATH"
  chmod 0644 "$PLIST_PATH"
  if command -v plutil >/dev/null 2>&1; then
    plutil -lint "$PLIST_PATH" >/dev/null || fail "generated launchd plist is invalid"
  fi
}

write_service_definition() {
  if [ "$SERVICE_KIND" = "systemd" ]; then
    write_systemd_unit
    return 0
  fi

  : >>"$LAUNCHD_STDOUT_PATH"
  : >>"$LAUNCHD_STDERR_PATH"
  chown root:"$SERVICE_GROUP" "$LAUNCHD_STDOUT_PATH" "$LAUNCHD_STDERR_PATH"
  chmod 0600 "$LAUNCHD_STDOUT_PATH" "$LAUNCHD_STDERR_PATH"
  write_launchd_plist
}

restart_systemd_service() {
  systemctl daemon-reload
  cleanup_legacy_auto_update
  systemctl daemon-reload
  systemctl enable nodelite-agent.service
  systemctl restart nodelite-agent.service
}

restart_launchd_service() {
  launchctl bootout system "$PLIST_PATH" >/dev/null 2>&1 || \
    launchctl bootout "system/$LAUNCHD_LABEL" >/dev/null 2>&1 || true
  launchctl bootstrap system "$PLIST_PATH" || fail "failed to bootstrap launchd service"
  launchctl enable "system/$LAUNCHD_LABEL" >/dev/null 2>&1 || true
  launchctl kickstart -k "system/$LAUNCHD_LABEL" || fail "failed to start launchd service"
}

restart_service() {
  if [ "$SERVICE_KIND" = "systemd" ]; then
    restart_systemd_service
    return 0
  fi

  restart_launchd_service
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --bootstrap-url)
      [ "$#" -ge 2 ] || fail "--bootstrap-url requires a value"
      BOOTSTRAP_URL="$2"
      shift 2
      ;;
    --install-token)
      [ "$#" -ge 2 ] || fail "--install-token requires a value"
      INSTALL_TOKEN="$2"
      shift 2
      ;;
    --install-token-file)
      [ "$#" -ge 2 ] || fail "--install-token-file requires a value"
      INSTALL_TOKEN_FILE="$2"
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || fail "--install-dir requires a value"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --config-dir)
      [ "$#" -ge 2 ] || fail "--config-dir requires a value"
      CONFIG_DIR="$2"
      shift 2
      ;;
    --mode)
      [ "$#" -ge 2 ] || fail "--mode requires a value"
      MODE="$2"
      shift 2
      ;;
    --base-url)
      [ "$#" -ge 2 ] || fail "--base-url requires a value"
      BASE_URL="$2"
      shift 2
      ;;
    --checksums-url)
      [ "$#" -ge 2 ] || fail "--checksums-url requires a value"
      CHECKSUMS_URL="$2"
      shift 2
      ;;
    --binary-url)
      [ "$#" -ge 2 ] || fail "--binary-url requires a value"
      BINARY_URL="$2"
      shift 2
      ;;
    --sha256-x86_64)
      [ "$#" -ge 2 ] || fail "--sha256-x86_64 requires a value"
      SHA256_X86_64="$2"
      shift 2
      ;;
    --sha256-aarch64)
      [ "$#" -ge 2 ] || fail "--sha256-aarch64 requires a value"
      SHA256_AARCH64="$2"
      shift 2
      ;;
    --help|-h)
      cat <<'EOF'
Usage:
  sh install-agent.sh \
    --bootstrap-url https://monitor.example.com/install/bootstrap

Custom binary URL:
  sh install-agent.sh \
    --bootstrap-url https://monitor.example.com/install/bootstrap \
    --binary-url https://your-host/releases/agent-linux-x86_64 \
    --sha256-x86_64 <64-character-sha256>

Optional:
  --install-token <one-time-token>
  --install-token-file <path>
  --install-dir <dir>
  --config-dir <dir>
  --mode <install|upgrade|auto>
  --base-url <release-base-url>
  --checksums-url <release-checksums-url>
  --binary-url <exact-binary-url>
  --sha256-x86_64 <sha256-override>
  --sha256-aarch64 <sha256-override>

Notes:
  This script no longer installs unattended auto-update timers. Use
  --mode upgrade manually, or trigger an upgrade from the authenticated
  dashboard after checking the target release.
  macOS uses an experimental launchd path and currently keeps the agent
  running as root instead of provisioning a dedicated service account.
EOF
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[ "$(id -u)" -eq 0 ] || fail "please run as root"

need_cmd uname
need_cmd curl
need_cmd awk
need_cmd grep
need_cmd id
need_cmd install
need_cmd mkdir
need_cmd mktemp
need_cmd mv
need_cmd rm
need_cmd sed
need_cmd chown
need_cmd chmod
configure_platform

if [ "$SERVICE_KIND" = "systemd" ]; then
  need_cmd systemctl
else
  need_cmd launchctl
fi

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)
    EXPECTED_SHA256="$SHA256_X86_64"
    if [ "$SERVICE_KIND" = "systemd" ]; then
      TARGET="x86_64-unknown-linux-musl"
    else
      TARGET="x86_64-apple-darwin"
    fi
    ;;
  aarch64|arm64)
    EXPECTED_SHA256="$SHA256_AARCH64"
    if [ "$SERVICE_KIND" = "systemd" ]; then
      TARGET="aarch64-unknown-linux-musl"
    else
      TARGET="aarch64-apple-darwin"
    fi
    ;;
  *)
    fail "unsupported architecture: $ARCH"
    ;;
esac
ARTIFACT_NAME="nodelite-agent-$TARGET"

if [ -n "$BINARY_URL" ]; then
  DOWNLOAD_URL="$BINARY_URL"
else
  resolve_release_base_url
  DOWNLOAD_URL="$BASE_URL/$ARTIFACT_NAME"
fi

BIN_PATH="$INSTALL_DIR/nodelite-agent"
CONFIG_PATH="$CONFIG_DIR/agent.toml"

existing_install=0
if [ -e "$CONFIG_PATH" ] || [ -e "$SERVICE_DEFINITION_PATH" ] || [ -e "$BIN_PATH" ]; then
  existing_install=1
fi

case "$MODE" in
  auto)
    if [ "$existing_install" -eq 1 ]; then
      MODE="upgrade"
    else
      MODE="install"
    fi
    ;;
  install|upgrade)
    ;;
  *)
    fail "--mode must be one of: install, upgrade, auto"
    ;;
esac

if [ "$MODE" = "install" ] && [ -z "$BOOTSTRAP_URL" ]; then
  fail "install mode requires --bootstrap-url"
fi

prepare_service_account
prepare_directories

TMP_PATH="$(mktemp "$INSTALL_DIR/nodelite-agent.XXXXXX")"
BOOTSTRAP_TMP="$(mktemp "$CONFIG_DIR/agent.toml.XXXXXX")"
CURL_AUTH_CONFIG="$(mktemp "$STATE_DIR/install-curl.XXXXXX")"
CHECKSUMS_TMP="$(mktemp "$STATE_DIR/install-sha256.XXXXXX")"

config_refreshed=0
if [ "$MODE" = "install" ] || [ -n "$BOOTSTRAP_URL" ]; then
  fetch_bootstrap_config
  config_refreshed=1
elif [ ! -f "$CONFIG_PATH" ]; then
  fail "upgrade mode requires an existing $CONFIG_PATH or a bootstrap URL to recreate it"
fi

fetch_expected_sha256 "$ARTIFACT_NAME"

printf '%s\n' "Downloading $DOWNLOAD_URL"
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_PATH" || fail "failed to download agent binary"
ACTUAL_SHA256="$(calculate_sha256 "$TMP_PATH")"
[ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ] || fail "downloaded agent checksum mismatch"

install -o root -g root -m 0755 "$TMP_PATH" "$BIN_PATH"
install_agent_config
write_service_definition
restart_service

if [ "$MODE" = "upgrade" ]; then
  printf '%s\n' "NodeLite agent upgraded and restarted."
else
  printf '%s\n' "NodeLite agent installed and started."
fi
printf '%s\n' "Config: $CONFIG_PATH"
if [ "$SERVICE_KIND" = "launchd" ]; then
  printf '%s\n' "Service: $SERVICE_STATUS_NAME (experimental macOS launchd support)"
  printf '%s\n' "Plist: $PLIST_PATH"
  printf '%s\n' "Logs: $LAUNCHD_STDOUT_PATH / $LAUNCHD_STDERR_PATH"
else
  printf '%s\n' "Service: $SERVICE_STATUS_NAME"
fi
