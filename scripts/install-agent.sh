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
BINARY_SHA256_X86_64=""
BINARY_SHA256_AARCH64=""
CA_URL=""
CA_B64=""
CA_SHA256=""
AGENT_VERSION=""
BOOTSTRAP_URL=""
BOOTSTRAP_SHA256=""
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
    --binary-sha256-x86-64) BINARY_SHA256_X86_64="${2:-}"; shift 2 ;;
    --binary-sha256-aarch64) BINARY_SHA256_AARCH64="${2:-}"; shift 2 ;;
    --ca-url) CA_URL="${2:-}"; shift 2 ;;
    --ca-b64) CA_B64="${2:-}"; shift 2 ;;
    --ca-sha256) CA_SHA256="${2:-}"; shift 2 ;;
    --agent-version) AGENT_VERSION="${2:-}"; shift 2 ;;
    --bootstrap-url) BOOTSTRAP_URL="${2:-}"; shift 2 ;;
    --bootstrap-sha256) BOOTSTRAP_SHA256="${2:-}"; shift 2 ;;
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

if [ -z "$BOOTSTRAP_URL" ] && [ -n "$BOOTSTRAP_SHA256" ]; then
  fail "--bootstrap-sha256 requires --bootstrap-url"
fi
if [ -n "$BOOTSTRAP_URL" ]; then
  case "$BOOTSTRAP_URL" in http://*|https://*) ;; *) fail "--bootstrap-url must use HTTP(S)" ;; esac
  printf '%s' "$BOOTSTRAP_URL" | grep -Eq '^https?://[^[:space:]"\\]+$' || \
    fail "invalid --bootstrap-url"
  printf '%s' "$BOOTSTRAP_SHA256" | grep -Eq '^[0-9a-fA-F]{64}$' || \
    fail "invalid --bootstrap-sha256"
  command -v curl >/dev/null 2>&1 || fail "curl is required for bootstrap installation"
  bootstrap_file="$(mktemp)"
  trap 'rm -f "$bootstrap_file"' EXIT INT TERM
  curl --fail --silent --show-error --location --proto '=http,https' --tlsv1.2 \
    "$BOOTSTRAP_URL" --output "$bootstrap_file"
  actual_bootstrap_sha256="$(sha256sum "$bootstrap_file" | awk '{print $1}')"
  [ "$actual_bootstrap_sha256" = "$(printf '%s' "$BOOTSTRAP_SHA256" | tr 'A-F' 'a-f')" ] || \
    fail "Agent bootstrap SHA-256 mismatch"
  manifest_field() {
    awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1)}' "$bootstrap_file"
  }
  PANEL_ENDPOINT="$(manifest_field panel_endpoint)"
  ENROLLMENT_ENDPOINT="$(manifest_field enrollment_endpoint)"
  SERVER_NAME="$(manifest_field server_name)"
  BINARY_URL="$(manifest_field binary_url)"
  BINARY_SHA256_X86_64="$(manifest_field binary_sha256_x86_64)"
  BINARY_SHA256_AARCH64="$(manifest_field binary_sha256_aarch64)"
  AGENT_VERSION="$(manifest_field binary_version)"
  CA_URL="$(manifest_field ca_url)"
  CA_SHA256="$(manifest_field ca_sha256)"
  rm -f "$bootstrap_file"
  trap - EXIT INT TERM
fi

case "$(uname -m)" in
  x86_64|amd64) AGENT_ARCH="x86_64" ;;
  aarch64|arm64) AGENT_ARCH="aarch64" ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

# Pick the architecture-specific pinned digest when one is provided.
if [ "$AGENT_ARCH" = "x86_64" ] && [ -n "$BINARY_SHA256_X86_64" ]; then
  BINARY_SHA256="$BINARY_SHA256_X86_64"
elif [ "$AGENT_ARCH" = "aarch64" ] && [ -n "$BINARY_SHA256_AARCH64" ]; then
  BINARY_SHA256="$BINARY_SHA256_AARCH64"
fi
BINARY_URL="$(printf '%s' "$BINARY_URL" | sed "s/{arch}/$AGENT_ARCH/g")"

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
  if [ -n "$CA_B64" ]; then
    printf '%s' "$CA_B64" | grep -Eq '^[A-Za-z0-9+/=_-]+$' || fail "invalid --ca-b64"
  else
    case "$CA_URL" in
      https://*) ;;
      http://*) [ -n "$CA_SHA256" ] || fail "HTTP --ca-url requires --ca-sha256" ;;
      *) fail "--ca-url must use HTTP(S), or pass --ca-b64" ;;
    esac
  fi
  if [ -n "$CA_SHA256" ]; then
    printf '%s' "$CA_SHA256" | grep -Eq '^[0-9a-fA-F]{64}$' || fail "invalid --ca-sha256"
  fi
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

download_ca() {
  case "$CA_URL" in
    https://*) download "$CA_URL" "$1" ;;
    http://*)
      command -v curl >/dev/null 2>&1 || fail "curl is required for a pinned HTTP CA download"
      curl --fail --silent --show-error --location --proto '=http' \
        "$CA_URL" --output "$1"
      ;;
  esac
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

machine_id="$(cat /etc/machine-id 2>/dev/null || hostname)"
agent_id="agent-$(printf '%s:%s' "$machine_id" "$NODE_ID" | sha256sum | cut -c1-24)"
if [ -f /var/lib/xenon/agent/agent.toml ]; then
  existing_agent_id="$(sed -n 's/^agent_id = "\([^"]*\)"$/\1/p' \
    /var/lib/xenon/agent/agent.toml | head -n 1)"
  existing_node_id="$(sed -n 's/^node_id = "\([^"]*\)"$/\1/p' \
    /var/lib/xenon/agent/agent.toml | head -n 1)"
  if [ "$existing_agent_id" = "xenon-local-test-agent" ] && \
    [ "$existing_node_id" = "xenon-local-test-node" ]; then
    printf 'Replacing the local test Agent configuration with production enrollment.\n'
    systemctl stop xenon-agent.service >/dev/null 2>&1 || true
    rm -rf /var/lib/xenon/agent/tls
    rm -f /var/lib/xenon/agent/traffic-spool.json
  elif [ "$existing_agent_id" != "$agent_id" ] || [ "$existing_node_id" != "$NODE_ID" ]; then
    fail "this VPS belongs to another managed host ($existing_node_id); uninstall it before rebinding"
  else
    printf 'Repairing the existing Agent configuration for host %s.\n' "$NODE_ID"
    systemctl stop xenon-agent.service >/dev/null 2>&1 || true
  fi
fi

if [ -n "$CA_B64" ]; then
  printf '%s' "$CA_B64" | base64 -d > "$tmp_dir/panel-ca.crt" 2>/dev/null || \
    fail "--ca-b64 is not valid base64"
  grep -q "BEGIN CERTIFICATE" "$tmp_dir/panel-ca.crt" || \
    fail "--ca-b64 does not decode to a PEM certificate"
else
  download_ca "$tmp_dir/panel-ca.crt"
fi
if [ -n "$CA_SHA256" ]; then
  actual_ca_sha256="$(sha256sum "$tmp_dir/panel-ca.crt" | awk '{print $1}')"
  [ "$actual_ca_sha256" = "$(printf '%s' "$CA_SHA256" | tr 'A-F' 'a-f')" ] || \
    fail "Panel CA SHA-256 mismatch"
fi

install -d -o root -g root -m 0755 /var/lib/xenon
if ! id xenon-agent >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/xenon/agent --create-home \
    --shell /usr/sbin/nologin xenon-agent
fi
install -o root -g root -m 0755 "$tmp_dir/xenon-agent" /usr/local/bin/xenon-agent
install -d -o xenon-agent -g xenon-agent -m 0700 /var/lib/xenon/agent/tls
install -o xenon-agent -g xenon-agent -m 0600 \
  "$tmp_dir/panel-ca.crt" /var/lib/xenon/agent/tls/panel-ca.crt

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
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
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
