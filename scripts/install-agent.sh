#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

PANEL_ENDPOINT=""
ENROLLMENT_ENDPOINT=""
SERVER_NAME=""
NODE_ID=""
TOKEN=""
BINARY_URL=""
BINARY_SHA256=""
CA_URL=""
AGENT_VERSION=""
UNINSTALL=0
UPGRADE=0
ROLLBACK=0

fail() {
  printf 'install-agent: %s\n' "$1" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --panel) PANEL_ENDPOINT="${2:-}"; shift 2 ;;
    --enrollment) ENROLLMENT_ENDPOINT="${2:-}"; shift 2 ;;
    --server-name) SERVER_NAME="${2:-}"; shift 2 ;;
    --node) NODE_ID="${2:-}"; shift 2 ;;
    --token) TOKEN="${2:-}"; shift 2 ;;
    --binary-url) BINARY_URL="${2:-}"; shift 2 ;;
    --binary-sha256) BINARY_SHA256="${2:-}"; shift 2 ;;
    --ca-url) CA_URL="${2:-}"; shift 2 ;;
    --agent-version) AGENT_VERSION="${2:-}"; shift 2 ;;
    --upgrade) UPGRADE=1; shift ;;
    --rollback) ROLLBACK=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || fail "run as root"
[ "$(uname -s)" = "Linux" ] || fail "Linux is required"
command -v systemctl >/dev/null 2>&1 || fail "systemd is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"

[ $((UNINSTALL + UPGRADE + ROLLBACK)) -le 1 ] || fail "choose only one operation"

if [ "$UNINSTALL" -eq 1 ]; then
  systemctl disable --now xenon-agent.service >/dev/null 2>&1 || true
  rm -f /etc/systemd/system/xenon-agent.service /usr/local/bin/xenon-agent
  rm -rf /var/lib/xenon/agent
  systemctl daemon-reload
  exit 0
fi

case "$(uname -m)" in
  x86_64|amd64|aarch64|arm64) ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

wait_for_service() {
  stable=0
  for _ in $(seq 1 30); do
    if systemctl is-active --quiet xenon-agent.service; then
      stable=$((stable + 1))
      [ "$stable" -ge 5 ] && return 0
    else
      stable=0
    fi
    sleep 1
  done
  return 1
}

if [ "$ROLLBACK" -eq 1 ]; then
  previous="/usr/local/lib/xenon-agent/xenon-agent.previous"
  current="/usr/local/bin/xenon-agent"
  [ -x "$previous" ] || fail "no previous Agent binary is available"
  [ -x "$current" ] || fail "current Agent binary is missing"
  swap="/usr/local/lib/xenon-agent/xenon-agent.rollback-swap"
  install -o root -g root -m 0755 "$current" "$swap"
  systemctl stop xenon-agent.service
  install -o root -g root -m 0755 "$previous" "$current"
  if systemctl start xenon-agent.service && wait_for_service; then
    install -o root -g root -m 0755 "$swap" "$previous"
    rm -f "$swap"
    printf 'Agent rollback completed.\n'
    exit 0
  fi
  systemctl stop xenon-agent.service >/dev/null 2>&1 || true
  install -o root -g root -m 0755 "$swap" "$current"
  rm -f "$swap"
  systemctl start xenon-agent.service || true
  fail "rollback candidate failed; original Agent was restored"
fi

if [ "$UPGRADE" -eq 0 ]; then
  case "$PANEL_ENDPOINT" in https://*) ;; *) fail "--panel must use https://" ;; esac
  case "$ENROLLMENT_ENDPOINT" in https://*) ;; *) fail "--enrollment must use https://" ;; esac
  case "$CA_URL" in https://*) ;; *) fail "--ca-url must use https://" ;; esac
  printf '%s' "$PANEL_ENDPOINT" | grep -Eq '^https://[^[:space:]"\\]+$' || fail "invalid --panel"
  printf '%s' "$ENROLLMENT_ENDPOINT" | grep -Eq '^https://[^[:space:]"\\]+$' || fail "invalid --enrollment"
  printf '%s' "$NODE_ID" | grep -Eq '^[A-Za-z0-9._:-]{1,128}$' || fail "invalid --node"
  printf '%s' "$TOKEN" | grep -Eq '^[A-Za-z0-9_-]{32,256}$' || fail "invalid --token"
  printf '%s' "$SERVER_NAME" | grep -Eq '^[A-Za-z0-9.-]{1,253}$' || fail "invalid --server-name"
else
  [ -f /var/lib/xenon/agent/agent.toml ] || fail "Agent is not installed"
  [ -x /usr/local/bin/xenon-agent ] || fail "current Agent binary is missing"
fi

case "$BINARY_URL" in https://*) ;; *) fail "--binary-url must use https://" ;; esac
printf '%s' "$BINARY_SHA256" | grep -Eq '^[0-9a-fA-F]{64}$' || fail "invalid --binary-sha256"
if [ -n "$AGENT_VERSION" ]; then
  printf '%s' "$AGENT_VERSION" | grep -Eq '^[0-9A-Za-z.+-]{1,64}$' || fail "invalid --agent-version"
fi

download() {
  url="$1"
  destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      "$url" --output "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only --quiet "$url" --output-document="$destination"
  else
    fail "curl or wget is required"
  fi
}

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT INT TERM
download "$BINARY_URL" "$tmp_dir/xenon-agent"
actual_sha256="$(sha256sum "$tmp_dir/xenon-agent" | awk '{print $1}')"
[ "$actual_sha256" = "$(printf '%s' "$BINARY_SHA256" | tr 'A-F' 'a-f')" ] || \
  fail "Agent binary SHA-256 mismatch"
chmod 0755 "$tmp_dir/xenon-agent"

version_info="$($tmp_dir/xenon-agent version-info 2>/dev/null)" || fail "candidate Agent version-info failed"
field() { printf '%s\n' "$version_info" | awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1)}'; }
candidate_version="$(field agent_version)"
[ -n "$candidate_version" ] || fail "candidate Agent did not report agent_version"
[ "$(field protocol_version)" = "0.1" ] || fail "candidate Agent protocol version is incompatible"
[ "$(field max_xray_version)" = "26.6.27" ] || fail "candidate Agent Xray policy is incompatible"
[ "$(field embedded_xray_version)" = "26.6.27" ] || fail "candidate Agent embeds an unsupported Xray version"
[ "$(field embedded_xray_available)" = "true" ] || fail "candidate Agent has no embedded Xray core"
[ -z "$AGENT_VERSION" ] || [ "$candidate_version" = "$AGENT_VERSION" ] || \
  fail "candidate Agent version $candidate_version does not match --agent-version $AGENT_VERSION"

if [ "$UPGRADE" -eq 1 ]; then
  previous="/usr/local/lib/xenon-agent/xenon-agent.previous"
  current="/usr/local/bin/xenon-agent"
  install -d -o root -g root -m 0755 /usr/local/lib/xenon-agent
  install -o root -g root -m 0755 "$current" "$previous"
  systemctl stop xenon-agent.service
  install -o root -g root -m 0755 "$tmp_dir/xenon-agent" "$current"
  if systemctl start xenon-agent.service && wait_for_service; then
    printf 'Agent upgraded to %s. Previous binary: %s\n' "$candidate_version" "$previous"
    exit 0
  fi
  systemctl stop xenon-agent.service >/dev/null 2>&1 || true
  install -o root -g root -m 0755 "$previous" "$current"
  if systemctl start xenon-agent.service && wait_for_service; then
    fail "upgrade to $candidate_version failed; previous Agent was restored"
  fi
  fail "upgrade failed and previous Agent could not be started"
fi

if [ -f /var/lib/xenon/agent/agent.toml ]; then
  installed_sha256="$(sha256sum /usr/local/bin/xenon-agent | awk '{print $1}')"
  [ "$installed_sha256" = "$(printf '%s' "$BINARY_SHA256" | tr 'A-F' 'a-f')" ] || \
    fail "Agent is already installed with another binary; use --upgrade"
  systemctl enable --now xenon-agent.service
  printf 'Agent with version %s is already installed; nothing changed.\n' "$candidate_version"
  exit 0
fi

download "$CA_URL" "$tmp_dir/panel-ca.crt"

if ! id xenon-agent >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/xenon/agent --create-home \
    --shell /usr/sbin/nologin xenon-agent
fi
install -o root -g root -m 0755 "$tmp_dir/xenon-agent" /usr/local/bin/xenon-agent
install -d -o xenon-agent -g xenon-agent -m 0700 /var/lib/xenon/agent/tls
install -o xenon-agent -g xenon-agent -m 0600 \
  "$tmp_dir/panel-ca.crt" /var/lib/xenon/agent/tls/panel-ca.crt

machine_id="$(cat /etc/machine-id 2>/dev/null || hostname)"
agent_id="agent-$(printf '%s:%s' "$machine_id" "$NODE_ID" | sha256sum | cut -c1-24)"
cat > /var/lib/xenon/agent/agent.toml <<EOF
panel_endpoint = "$PANEL_ENDPOINT"
agent_id = "$agent_id"
node_id = "$NODE_ID"
registration_token = "$TOKEN"
interval_seconds = 10

[tls]
enabled = true
ca_path = "/var/lib/xenon/agent/tls/panel-ca.crt"
cert_path = "/var/lib/xenon/agent/tls/agent.crt"
key_path = "/var/lib/xenon/agent/tls/agent.key"
domain_name = "$SERVER_NAME"
enrollment_endpoint = "$ENROLLMENT_ENDPOINT"
renew_before_days = 14

[xray]
api_endpoint = "http://127.0.0.1:10085"
inbound_tag = "vless-in"
listen_address = "0.0.0.0"
listen_port = 443
protocol = "vless"
transport = "tcp"
security = "none"

[spool]
path = "/var/lib/xenon/agent/traffic-spool.json"
max_batches = 2048
max_bytes = 16777216
EOF
chown xenon-agent:xenon-agent /var/lib/xenon/agent/agent.toml
chmod 0600 /var/lib/xenon/agent/agent.toml

cat > /etc/systemd/system/xenon-agent.service <<'EOF'
[Unit]
Description=Xray Agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=xenon-agent
Group=xenon-agent
WorkingDirectory=/var/lib/xenon/agent
Environment=AGENT_CONFIG=/var/lib/xenon/agent/agent.toml
ExecStart=/usr/local/bin/xenon-agent
Restart=always
RestartSec=1s
KillMode=control-group
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/xenon/agent

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now xenon-agent.service

for _ in $(seq 1 30); do
  if grep -Eq '^registration_token = ""$' /var/lib/xenon/agent/agent.toml; then
    printf 'Agent enrolled and started.\n'
    exit 0
  fi
  sleep 1
done
fail "Agent did not enroll within 30 seconds; inspect journalctl -u xenon-agent"
