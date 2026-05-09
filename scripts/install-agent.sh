#!/bin/sh
set -eu
umask 077

fail() {
  printf '%s\n' "install-agent: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

BOOTSTRAP_URL=""
INSTALL_TOKEN="${XIMONITOR_AGENT_INSTALL_TOKEN:-}"
INSTALL_TOKEN_FILE="${XIMONITOR_AGENT_INSTALL_TOKEN_FILE:-}"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/ximonitor"
BASE_URL="${XIMONITOR_AGENT_BASE_URL:-https://example.invalid/ximonitor/releases/latest/download}"
BINARY_URL="${XIMONITOR_AGENT_BINARY_URL:-}"
SHA256_X86_64="${XIMONITOR_AGENT_SHA256_X86_64:-}"
SHA256_AARCH64="${XIMONITOR_AGENT_SHA256_AARCH64:-}"
SERVICE_USER="ximonitor-agent"
SERVICE_GROUP="ximonitor-agent"
STATE_DIR="/var/lib/ximonitor-agent"
BIN_PATH=""
CONFIG_PATH=""
UNIT_PATH="/etc/systemd/system/ximonitor-agent.service"
TMP_PATH=""
BOOTSTRAP_TMP=""
CURL_AUTH_CONFIG=""

cleanup() {
  [ -n "$TMP_PATH" ] && rm -f "$TMP_PATH"
  [ -n "$BOOTSTRAP_TMP" ] && rm -f "$BOOTSTRAP_TMP"
  [ -n "$CURL_AUTH_CONFIG" ] && rm -f "$CURL_AUTH_CONFIG"
}

trap cleanup EXIT HUP INT TERM

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

load_install_token() {
  if [ -n "$INSTALL_TOKEN_FILE" ]; then
    [ -r "$INSTALL_TOKEN_FILE" ] || fail "install token file is not readable: $INSTALL_TOKEN_FILE"
    INSTALL_TOKEN="$(sed -n '1p' "$INSTALL_TOKEN_FILE")"
  elif [ -z "$INSTALL_TOKEN" ]; then
    prompt_install_token
  fi

  [ -n "$INSTALL_TOKEN" ] || fail "install token must not be empty"
}

write_curl_auth_config() {
  cat >"$CURL_AUTH_CONFIG" <<EOF
header = "Authorization: Bearer $INSTALL_TOKEN"
EOF
  chmod 0600 "$CURL_AUTH_CONFIG"
}

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
    --base-url)
      [ "$#" -ge 2 ] || fail "--base-url requires a value"
      BASE_URL="$2"
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
    --bootstrap-url https://monitor.example.com/install/bootstrap \
    --sha256-x86_64 <sha256> \
    --sha256-aarch64 <sha256>

Optional:
  --install-token <one-time-token>
  --install-token-file <path>
  --install-dir <dir>
  --config-dir <dir>
  --base-url <release-base-url>
  --binary-url <exact-binary-url>
EOF
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[ "$(id -u)" -eq 0 ] || fail "please run as root"
[ -n "$BOOTSTRAP_URL" ] || fail "missing --bootstrap-url"

need_cmd uname
need_cmd curl
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
need_cmd systemctl

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)
    TARGET="x86_64-unknown-linux-musl"
    EXPECTED_SHA256="$SHA256_X86_64"
    ;;
  aarch64|arm64)
    TARGET="aarch64-unknown-linux-musl"
    EXPECTED_SHA256="$SHA256_AARCH64"
    ;;
  *)
    fail "unsupported architecture: $ARCH"
    ;;
esac

[ -n "$EXPECTED_SHA256" ] || fail "missing expected sha256 for target $TARGET"

if [ -n "$BINARY_URL" ]; then
  DOWNLOAD_URL="$BINARY_URL"
else
  DOWNLOAD_URL="$BASE_URL/ximonitor-agent-$TARGET"
fi

BIN_PATH="$INSTALL_DIR/ximonitor-agent"
CONFIG_PATH="$CONFIG_DIR/agent.toml"

ensure_service_account
SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$STATE_DIR"
chown root:root "$INSTALL_DIR"
chmod 0755 "$INSTALL_DIR"
chown root:"$SERVICE_GROUP" "$CONFIG_DIR" "$STATE_DIR"
chmod 0750 "$CONFIG_DIR" "$STATE_DIR"

TMP_PATH="$(mktemp "$INSTALL_DIR/ximonitor-agent.XXXXXX")"
BOOTSTRAP_TMP="$(mktemp "$CONFIG_DIR/agent.toml.XXXXXX")"
CURL_AUTH_CONFIG="$(mktemp "$STATE_DIR/install-curl.XXXXXX")"

fetch_bootstrap_config

printf '%s\n' "Downloading $DOWNLOAD_URL"
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_PATH" || fail "failed to download agent binary"
ACTUAL_SHA256="$(calculate_sha256 "$TMP_PATH")"
[ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ] || fail "downloaded agent checksum mismatch"

install -o root -g root -m 0755 "$TMP_PATH" "$BIN_PATH"
install -o root -g "$SERVICE_GROUP" -m 0640 "$BOOTSTRAP_TMP" "$CONFIG_PATH"

cat >"$UNIT_PATH" <<EOF
[Unit]
Description=XiMonitor Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN_PATH --config $CONFIG_PATH
Restart=always
RestartSec=3
User=$SERVICE_USER
Group=$SERVICE_GROUP
WorkingDirectory=$STATE_DIR
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=full

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable ximonitor-agent.service
systemctl restart ximonitor-agent.service

printf '%s\n' "XiMonitor agent installed and started."
printf '%s\n' "Config: $CONFIG_PATH"
printf '%s\n' "Service: ximonitor-agent.service"
