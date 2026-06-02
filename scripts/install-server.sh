#!/bin/sh
# NodeLite Server 一键安装 / 升级 / 迁移脚本。
#
# 主要流程:
#   1. 解析参数与环境变量,确定模式(install / upgrade / migrate / auto)。
#   2. 探测当前是否已存在安装(unit 文件、配置文件、二进制),据此推断默认值。
#   3. 拉取并校验对应架构的发布二进制,落到 /usr/local/bin。
#   4. 生成或保留 server.toml / server.json,写入 systemd unit,重启服务。
#
# 设计目标与 install-agent.sh 一致:全部 POSIX shell、不依赖 bash。

set -eu
# 默认 umask:确保临时文件不会泄漏给同主机其它用户。
umask 077

# VERSION 环境变量:默认只安装正式版本。如需 alpha/beta/RC 等预发布版本,
# 必须显式设置 VERSION=v2.3.0-alpha.1,否则安装脚本会拒绝。
# 留空时自动使用 GitHub 最新正式版。
VERSION="${NODELITE_SERVER_VERSION:-}"
if [ -n "$VERSION" ]; then
  BASE_URL="${NODELITE_SERVER_BASE_URL:-https://github.com/XiNian-dada/NodeLite/releases/download/${VERSION}}"
else
  BASE_URL="${NODELITE_SERVER_BASE_URL:-https://github.com/XiNian-dada/NodeLite/releases/latest/download}"
fi
INSTALL_ROOT_DEFAULT="/opt/nodelite"
LISTEN_HOST_DEFAULT="127.0.0.1"
SERVICE_NAME="nodelite-server"
BIN_PATH="/usr/local/bin/nodelite-server"
UNIT_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
MODE="${NODELITE_SERVER_MODE:-auto}"

TMP_BIN=""
TMP_SHA256=""

# 统一的错误输出函数。
fail() {
  printf '%s\n' "install-server: $*" >&2
  exit 1
}

# 依赖命令检查。
need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

# 退出时清理临时文件。
cleanup() {
  [ -n "$TMP_BIN" ] && rm -f "$TMP_BIN"
  [ -n "$TMP_SHA256" ] && rm -f "$TMP_SHA256"
}

trap cleanup EXIT HUP INT TERM

# 检测当前脚本是否跑在交互式终端中。这里不能主动触碰 `/dev/tty`,
# 否则像网页触发升级这种无控制终端场景会在探测阶段就被 shell 报错打断。
has_tty() {
  [ -t 0 ] || [ -t 1 ] || [ -t 2 ]
}

# 统一输出一整行文本。
tty_println() {
  if has_tty; then
    printf '%s\n' "$*" >/dev/tty
  else
    printf '%s\n' "$*"
  fi
}

# 清屏:优先使用 `clear`,否则发送 ANSI 序列。
clear_screen() {
  if ! has_tty; then
    return 0
  fi
  case "${TERM:-}" in
    ""|dumb)
      return 0
      ;;
  esac
  if command -v clear >/dev/null 2>&1; then
    clear >/dev/tty 2>/dev/null || true
    return 0
  fi
  printf '\033c' >/dev/tty 2>/dev/null || true
}

# 通用交互式读取:提供默认值,空输入将沿用默认值。
read_line() {
  prompt="$1"
  default_value="$2"

  [ -r /dev/tty ] || fail "interactive input requires a controlling terminal"
  if [ -n "$default_value" ]; then
    printf '%s [%s]: ' "$prompt" "$default_value" >/dev/tty
  else
    printf '%s: ' "$prompt" >/dev/tty
  fi
  IFS= read -r value </dev/tty || fail "failed to read user input"
  if [ -z "$value" ]; then
    value="$default_value"
  fi
  printf '%s' "$value"
}

# 必填字段:空输入时循环提示。
prompt_required() {
  prompt="$1"
  default_value="$2"

  while :; do
    value="$(read_line "$prompt" "$default_value")"
    if [ -n "$value" ]; then
      printf '%s' "$value"
      return 0
    fi
    printf '%s\n' "This field is required." >/dev/tty
  done
}

# 校验运行模式。auto 表示按已存在文件自动判断 install / upgrade。
validate_mode() {
  case "$1" in
    auto|install|upgrade|migrate)
      return 0
      ;;
    *)
      fail "operation mode must be auto, install, upgrade, or migrate"
      ;;
  esac
}

# 默认为 n 的确认提示:用于覆盖现有安装等危险操作。
confirm_default_no() {
  prompt="$1"

  while :; do
    answer="$(read_line "$prompt" "n")"
    case "$answer" in
      y|Y|yes|YES)
        return 0
        ;;
      n|N|no|NO)
        return 1
        ;;
      *)
        printf '%s\n' "Please answer y or n." >/dev/tty
        ;;
    esac
  done
}

# 从 /dev/urandom 中生成指定字节数的十六进制串。
random_hex() {
  bytes="$1"
  od -An -N"$bytes" -tx1 /dev/urandom | tr -d ' \n'
}

# 从给定字符集中随机抽取一个字符。故意只依赖 `od` / `awk`,避免 GNU `shuf`
# 这类在 BusyBox/Alpine 上不一定存在的工具。
random_char_from_set() {
  charset="$1"
  charset_length="$(printf '%s' "$charset" | awk '{ print length }')"
  [ "$charset_length" -gt 0 ] || fail "random_char_from_set requires a non-empty charset"
  random_index="$(( $(od -An -N2 -tu2 /dev/urandom | tr -d ' \n') % charset_length + 1 ))"
  printf '%s' "$charset" | awk -v pos="$random_index" 'BEGIN { ORS="" } { print substr($0, pos, 1) }'
}

append_random_chars() {
  charset="$1"
  count="$2"
  result=""

  while [ "$count" -gt 0 ]; do
    result="${result}$(random_char_from_set "$charset")"
    count=$((count - 1))
  done

  printf '%s' "$result"
}

# 生成符合密码策略的强密码（至少12字符，包含大小写字母、数字、特殊字符）。
# 字符类型按交错顺序拼接,避免依赖额外的"洗牌"工具,同时仍保留足够熵。
generate_strong_password() {
  uppercase="ABCDEFGHJKLMNPQRSTUVWXYZ"  # 去除易混淆的 I O
  lowercase="abcdefghijkmnopqrstuvwxyz"  # 去除易混淆的 l
  digits="23456789"  # 去除易混淆的 0 1
  special="!@#\$%^&*-_=+"
  all_chars="${uppercase}${lowercase}${digits}${special}"

  password=""
  password="${password}$(append_random_chars "$uppercase" 1)"
  password="${password}$(append_random_chars "$lowercase" 1)"
  password="${password}$(append_random_chars "$digits" 1)"
  password="${password}$(append_random_chars "$special" 1)"
  password="${password}$(append_random_chars "$lowercase" 1)"
  password="${password}$(append_random_chars "$uppercase" 1)"
  password="${password}$(append_random_chars "$special" 1)"
  password="${password}$(append_random_chars "$digits" 1)"
  password="${password}$(append_random_chars "$all_chars" 8)"
  printf '%s' "$password"
}

# 在 [20000, 40000) 区间内随机一个端口,降低默认监听端口被占用的概率。
random_port() {
  raw_port="$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')"
  printf '%s' "$((20000 + raw_port % 20000))"
}

# 计算文件 SHA-256:优先 sha256sum,缺失时回退到 shasum -a 256。
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

# 拉取发布源的 SHA256SUMS.txt 并解析出当前 artifact 的预期摘要。
fetch_expected_sha256() {
  artifact_name="$1"
  checksums_url="$BASE_URL/SHA256SUMS.txt"

  printf '%s\n' "Fetching release checksums from $checksums_url" >&2
  curl -fsSL "$checksums_url" -o "$TMP_SHA256" || fail "failed to fetch release checksums"

  expected_sha256="$(awk -v artifact="$artifact_name" '
    NF >= 2 {
      path = $2
      sub(/^\*/, "", path)
      count = split(path, parts, "/")
      if (parts[count] == artifact) {
        print $1
        exit
      }
    }
  ' "$TMP_SHA256")"

  [ -n "$expected_sha256" ] || fail "missing checksum entry for $artifact_name"
  printf '%s' "$expected_sha256"
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

# 把 `releases/latest/download` 解析为具体 tag,固化本次安装版本。
# 如果用户通过 VERSION 环境变量指定了版本,则跳过解析直接使用。
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
        fail "resolved latest release '${resolved_tag}' is a pre-release. Set NODELITE_SERVER_VERSION=${resolved_tag} to install it explicitly."
      fi
      BASE_URL="${releases_root}/download/${resolved_tag}"
      printf '%s\n' "Resolved GitHub latest release tag: $resolved_tag"
      ;;
  esac
}

# 端口范围检查。
validate_port() {
  value="$1"
  case "$value" in
    ''|*[!0-9]*)
      fail "listen port must be a number between 1 and 65535"
      ;;
  esac
  if [ "$value" -lt 1 ] || [ "$value" -gt 65535 ]; then
    fail "listen port must be between 1 and 65535"
  fi
}

# 公网访问协议只允许 http / https。
validate_scheme() {
  value="$1"
  case "$value" in
    http|https)
      return 0
      ;;
    *)
      fail "public scheme must be http or https"
      ;;
  esac
}

# 避免 systemd unit 路径里出现空白带来的解析歧义。
validate_no_whitespace() {
  field="$1"
  value="$2"
  case "$value" in
    *[[:space:]]*)
      fail "$field must not contain whitespace"
      ;;
  esac
}

# 从已有 systemd unit 中提取 WorkingDirectory,作为升级 / 迁移的现有目录。
detect_existing_install_root() {
  if [ -r "$UNIT_PATH" ]; then
    awk -F= '/^WorkingDirectory=/{print $2; exit}' "$UNIT_PATH"
    return 0
  fi

  printf '%s' ""
}

# 把源目录下的全部内容拷到目标目录,保留点文件;源目录不存在时静默返回。
copy_tree_contents() {
  source_dir="$1"
  target_dir="$2"

  [ -d "$source_dir" ] || return 0
  mkdir -p "$target_dir"
  if [ -n "$(find "$source_dir" -mindepth 1 -maxdepth 1 2>/dev/null)" ]; then
    cp -R "$source_dir"/. "$target_dir"/
  fi
}

# 极简的 TOML 取值器:仅支持 `[section]` 下的"键 = 原始值"行,够升级流程用。
toml_get_raw() {
  file="$1"
  section="$2"
  key="$3"

  awk -v section="[$section]" -v key="$key" '
    /^\[/ {
      in_section = ($0 == section)
      next
    }
    in_section {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      if (line ~ "^" key "[[:space:]]*=") {
        sub(/^[^=]+=[[:space:]]*/, "", line)
        print line
        exit
      }
    }
  ' "$file"
}

trim_whitespace() {
  printf '%s' "$1" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

strip_toml_string_quotes() {
  value="$(trim_whitespace "$1")"
  case "$value" in
    \"*\")
      value="${value#\"}"
      value="${value%\"}"
      ;;
  esac
  printf '%s' "$value"
}

# 升级 / 迁移时从已有 server.toml 读出默认值,避免重置用户的自定义配置。
load_existing_server_defaults() {
  config_path="$1"
  [ -f "$config_path" ] || return 0

  listen_value="$(strip_toml_string_quotes "$(toml_get_raw "$config_path" server listen)")"
  if [ -n "$listen_value" ] && [ "$listen_value" != "${listen_value%:*}" ]; then
    LISTEN_HOST_DEFAULT_VALUE="${listen_value%:*}"
    LISTEN_PORT_DEFAULT_VALUE="${listen_value##*:}"
  fi

  public_base_url="$(strip_toml_string_quotes "$(toml_get_raw "$config_path" server public_base_url)")"
  case "$public_base_url" in
    http://*)
      PUBLIC_SCHEME_DEFAULT_VALUE="http"
      PUBLIC_HOST_DEFAULT_VALUE="${public_base_url#http://}"
      ;;
    https://*)
      PUBLIC_SCHEME_DEFAULT_VALUE="https"
      PUBLIC_HOST_DEFAULT_VALUE="${public_base_url#https://}"
      ;;
  esac

  readonly_username="$(strip_toml_string_quotes "$(toml_get_raw "$config_path" auth username)")"
  if [ -n "$readonly_username" ]; then
    READONLY_USERNAME_DEFAULT_VALUE="$readonly_username"
  fi
  readonly_password="$(strip_toml_string_quotes "$(toml_get_raw "$config_path" auth password)")"
  if [ -n "$readonly_password" ]; then
    READONLY_PASSWORD_DEFAULT_VALUE="$readonly_password"
  fi

  value="$(trim_whitespace "$(toml_get_raw "$config_path" server stale_after_secs)")"
  [ -n "$value" ] && SERVER_STALE_AFTER_SECS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" server ping_interval_secs)")"
  [ -n "$value" ] && SERVER_PING_INTERVAL_SECS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" server max_message_bytes)")"
  [ -n "$value" ] && SERVER_MAX_MESSAGE_BYTES="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ws max_total_connections)")"
  [ -n "$value" ] && WS_MAX_TOTAL_CONNECTIONS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ws max_connections_per_ip)")"
  [ -n "$value" ] && WS_MAX_CONNECTIONS_PER_IP="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ws auth_fail_window_secs)")"
  [ -n "$value" ] && WS_AUTH_FAIL_WINDOW_SECS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ws auth_fail_max_attempts)")"
  [ -n "$value" ] && WS_AUTH_FAIL_MAX_ATTEMPTS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ws auth_block_secs)")"
  [ -n "$value" ] && WS_AUTH_BLOCK_SECS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" ui refresh_interval_secs)")"
  [ -n "$value" ] && UI_REFRESH_INTERVAL_SECS="$value"
  value="$(trim_whitespace "$(toml_get_raw "$config_path" filters ignored_filesystems)")"
  [ -n "$value" ] && IGNORED_FILESYSTEMS_RAW="$value"
}

# 把交互或默认得到的变量拼成最终 server.toml 文本。
render_server_config() {
  insecure_allow_http_value="false"
  if [ "$PUBLIC_SCHEME" = "http" ]; then
    insecure_allow_http_value="true"
  fi
  cat <<EOF
[server]
listen = "${LISTEN_HOST}:${LISTEN_PORT}"
public_base_url = "${PUBLIC_SCHEME}://${PUBLIC_HOST}"
insecure_allow_http = ${insecure_allow_http_value}
node_registry_path = "${CONFIG_DIR}/server.json"
history_db_path = "${DATA_DIR}/history.sqlite3"
snapshot_path = "${DATA_DIR}/snapshot.json"
stale_after_secs = ${SERVER_STALE_AFTER_SECS}
ping_interval_secs = ${SERVER_PING_INTERVAL_SECS}
max_message_bytes = ${SERVER_MAX_MESSAGE_BYTES}

[auth]
username = "${READONLY_USERNAME}"
password = "${READONLY_PASSWORD}"

[ws]
max_total_connections = ${WS_MAX_TOTAL_CONNECTIONS}
max_connections_per_ip = ${WS_MAX_CONNECTIONS_PER_IP}
auth_fail_window_secs = ${WS_AUTH_FAIL_WINDOW_SECS}
auth_fail_max_attempts = ${WS_AUTH_FAIL_MAX_ATTEMPTS}
auth_block_secs = ${WS_AUTH_BLOCK_SECS}

[ui]
refresh_interval_secs = ${UI_REFRESH_INTERVAL_SECS}

[filters]
ignored_filesystems = ${IGNORED_FILESYSTEMS_RAW}
EOF
}

[ "$(id -u)" -eq 0 ] || fail "please run as root"

need_cmd awk
need_cmd chmod
need_cmd chown
need_cmd cp
need_cmd curl
need_cmd id
need_cmd install
need_cmd find
need_cmd mkdir
need_cmd mktemp
need_cmd mv
need_cmd od
need_cmd rm
need_cmd sed
need_cmd systemctl
need_cmd tr
need_cmd uname

clear_screen

tty_println "NodeLite Server Installer"
tty_println "This script installs the latest NodeLite server release from GitHub."
tty_println ""

validate_mode "$MODE"

existing_install_root="$(detect_existing_install_root)"
if [ -n "$existing_install_root" ]; then
  INSTALL_ROOT_DEFAULT="$existing_install_root"
fi

existing_install=0
if [ -n "$existing_install_root" ] || [ -e "$UNIT_PATH" ] || [ -e "$BIN_PATH" ]; then
  existing_install=1
fi

if [ "$MODE" = "auto" ]; then
  if [ "$existing_install" -eq 1 ]; then
    MODE="upgrade"
  else
    MODE="install"
  fi
fi

if [ "$MODE" = "upgrade" ] || [ "$MODE" = "migrate" ]; then
  if [ "$existing_install" -ne 1 ]; then
    fail "$MODE mode requires an existing NodeLite server installation"
  fi
  if [ -z "$existing_install_root" ]; then
    fail "failed to detect the current NodeLite install root from the systemd unit"
  fi
fi

CURRENT_INSTALL_ROOT="$existing_install_root"
CURRENT_CONFIG_PATH=""
CURRENT_REGISTRY_PATH=""
CURRENT_DATA_DIR=""
if [ -n "$CURRENT_INSTALL_ROOT" ]; then
  CURRENT_CONFIG_PATH="$CURRENT_INSTALL_ROOT/config/server.toml"
  CURRENT_REGISTRY_PATH="$CURRENT_INSTALL_ROOT/config/server.json"
  CURRENT_DATA_DIR="$CURRENT_INSTALL_ROOT/data"
fi

SERVER_STALE_AFTER_SECS="20"
SERVER_PING_INTERVAL_SECS="10"
SERVER_MAX_MESSAGE_BYTES="65536"
WS_MAX_TOTAL_CONNECTIONS="1024"
WS_MAX_CONNECTIONS_PER_IP="32"
WS_AUTH_FAIL_WINDOW_SECS="300"
WS_AUTH_FAIL_MAX_ATTEMPTS="12"
WS_AUTH_BLOCK_SECS="900"
UI_REFRESH_INTERVAL_SECS="5"
IGNORED_FILESYSTEMS_RAW='["tmpfs", "devtmpfs", "overlay"]'

LISTEN_HOST_DEFAULT_VALUE="$LISTEN_HOST_DEFAULT"
LISTEN_PORT_DEFAULT_VALUE="$(random_port)"
PUBLIC_HOST_DEFAULT_VALUE=""
PUBLIC_SCHEME_DEFAULT_VALUE="https"
READONLY_USERNAME_DEFAULT_VALUE="viewer"
READONLY_PASSWORD_DEFAULT_VALUE="$(generate_strong_password)"

if [ "$MODE" = "upgrade" ] || [ "$MODE" = "migrate" ]; then
  load_existing_server_defaults "$CURRENT_CONFIG_PATH"
fi

if [ "$MODE" = "upgrade" ]; then
  INSTALL_ROOT="$CURRENT_INSTALL_ROOT"
else
  INSTALL_ROOT="$(prompt_required "Install root directory" "$INSTALL_ROOT_DEFAULT")"
fi

CONFIG_DIR="$INSTALL_ROOT/config"
DATA_DIR="$INSTALL_ROOT/data"
CONFIG_PATH="$CONFIG_DIR/server.toml"
REGISTRY_PATH="$CONFIG_DIR/server.json"

if [ "$MODE" = "install" ] && [ "$existing_install" -eq 1 ]; then
  if ! confirm_default_no "Existing NodeLite files detected. Overwrite them?"; then
    fail "aborted by user"
  fi
fi

if [ "$MODE" = "upgrade" ]; then
  [ -f "$CONFIG_PATH" ] || fail "existing server config not found at $CONFIG_PATH"
else
  LISTEN_HOST="$(prompt_required "Listen host" "$LISTEN_HOST_DEFAULT_VALUE")"
  LISTEN_PORT="$(prompt_required "Listen port" "$LISTEN_PORT_DEFAULT_VALUE")"
  PUBLIC_HOST="$(prompt_required "Public domain or IP" "$PUBLIC_HOST_DEFAULT_VALUE")"
  PUBLIC_SCHEME="$(prompt_required "Public scheme" "$PUBLIC_SCHEME_DEFAULT_VALUE")"
  READONLY_USERNAME="$(prompt_required "Readonly username" "$READONLY_USERNAME_DEFAULT_VALUE")"
  READONLY_PASSWORD="$(prompt_required "Readonly password" "$READONLY_PASSWORD_DEFAULT_VALUE")"

  validate_port "$LISTEN_PORT"
  validate_scheme "$PUBLIC_SCHEME"
  validate_no_whitespace "install root directory" "$INSTALL_ROOT"
  validate_no_whitespace "public host" "$PUBLIC_HOST"
fi

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)
    TARGET="x86_64-unknown-linux-musl"
    ;;
  aarch64|arm64)
    TARGET="aarch64-unknown-linux-musl"
    ;;
  *)
    fail "unsupported architecture: $ARCH"
    ;;
esac

ARTIFACT_NAME="nodelite-server-$TARGET"
resolve_release_base_url
DOWNLOAD_URL="$BASE_URL/$ARTIFACT_NAME"

mkdir -p "$INSTALL_ROOT" "$CONFIG_DIR" "$DATA_DIR"
chown root:root "$INSTALL_ROOT" "$CONFIG_DIR" "$DATA_DIR"
chmod 0755 "$INSTALL_ROOT"
chmod 0700 "$CONFIG_DIR" "$DATA_DIR"

TMP_BIN="$(mktemp "$INSTALL_ROOT/nodelite-server.XXXXXX")"
TMP_SHA256="$(mktemp "$INSTALL_ROOT/nodelite-sha256.XXXXXX")"

EXPECTED_SHA256="$(fetch_expected_sha256 "$ARTIFACT_NAME")"

printf '%s\n' "Downloading $DOWNLOAD_URL"
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_BIN" || fail "failed to download server binary"
ACTUAL_SHA256="$(calculate_sha256 "$TMP_BIN")"
[ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ] || fail "downloaded server checksum mismatch"

install -o root -g root -m 0755 "$TMP_BIN" "$BIN_PATH"

if [ "$MODE" = "migrate" ] && [ "$CURRENT_INSTALL_ROOT" != "$INSTALL_ROOT" ]; then
  copy_tree_contents "$CURRENT_DATA_DIR" "$DATA_DIR"
  if [ -f "$CURRENT_REGISTRY_PATH" ]; then
    cp "$CURRENT_REGISTRY_PATH" "$REGISTRY_PATH"
  fi
fi

if [ "$MODE" != "upgrade" ]; then
  render_server_config >"$CONFIG_PATH"
  chmod 0600 "$CONFIG_PATH"

  if [ ! -f "$REGISTRY_PATH" ]; then
    printf '%s\n' '{"nodes":[],"install_sessions":[]}' >"$REGISTRY_PATH"
  fi
  chmod 0600 "$REGISTRY_PATH"
fi

cat >"$UNIT_PATH" <<EOF
[Unit]
Description=NodeLite Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN_PATH --config $CONFIG_PATH
Restart=always
RestartSec=3
TimeoutStopSec=15s
WorkingDirectory=$INSTALL_ROOT
User=root
Group=root
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
ReadWritePaths=$INSTALL_ROOT

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$SERVICE_NAME.service"
systemctl restart "$SERVICE_NAME.service"

clear_screen
if [ "$MODE" = "upgrade" ]; then
  tty_println "NodeLite server upgraded and restarted."
elif [ "$MODE" = "migrate" ]; then
  tty_println "NodeLite server migrated, upgraded, and restarted."
else
  tty_println "NodeLite server installed and started."
fi
tty_println "Binary: $BIN_PATH"
tty_println "Config: $CONFIG_PATH"
tty_println "Registry: $REGISTRY_PATH"
if [ "$MODE" = "upgrade" ]; then
  tty_println "Config preserved: existing server.toml and readonly credentials were kept."
else
  tty_println "Readonly username: $READONLY_USERNAME"
  tty_println "Readonly password: $READONLY_PASSWORD"
  tty_println "Public base URL: ${PUBLIC_SCHEME}://${PUBLIC_HOST}"
fi
tty_println ""
tty_println "Next step: enroll an agent from this server with:"
tty_println "  $BIN_PATH --config $CONFIG_PATH install-agent --node-id hk-01 --node-label \"Hong Kong 01\""
