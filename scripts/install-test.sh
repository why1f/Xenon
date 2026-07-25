#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() {
  printf 'install-test: %s\n' "$1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail "run as root"
[ "$(uname -s)" = "Linux" ] || fail "Linux is required"
command -v systemctl >/dev/null 2>&1 || fail "systemd is required"
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v grep >/dev/null 2>&1 || fail "grep is required"

port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") >/dev/null 2>&1
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bundle_dir="$(cd "$script_dir/.." && pwd)"
panel_binary="$bundle_dir/bin/xenon"
agent_binary="$bundle_dir/bin/xenon-agent"

[ -x "$panel_binary" ] || fail "bundle binary is missing: $panel_binary"
[ -x "$agent_binary" ] || fail "bundle binary is missing: $agent_binary"
[ -f "$bundle_dir/systemd/xenon.service" ] || fail "bundle Xenon service is missing"
[ -f "$bundle_dir/systemd/xenon-agent.service" ] || fail "bundle Agent service is missing"

printf '%s\n' \
  'WARNING: this installs an insecure loopback-only Xenon test environment.' \
  'Do not expose ports 50051 or 18181 publicly and do not use this mode in production.'

systemctl stop xenon-agent.service xenon.service >/dev/null 2>&1 || true
systemctl reset-failed xenon-agent.service xenon.service >/dev/null 2>&1 || true
if [ -f /etc/xenon/xenon.toml ] && \
  grep -Fxq 'http_addr = "127.0.0.1:18081"' /etc/xenon/xenon.toml; then
  sed -i 's/^http_addr = "127\.0\.0\.1:18081"$/http_addr = "127.0.0.1:18181"/' \
    /etc/xenon/xenon.toml
  chown root:xenon /etc/xenon/xenon.toml
  chmod 0640 /etc/xenon/xenon.toml
fi
for port in 50051 18181 10085 18443; do
  if port_in_use "$port"; then
    fail "TCP port $port is already in use after stopping Xenon services; inspect: ss -ltnp 'sport = :$port'"
  fi
done

install -d -o root -g root -m 0755 /var/lib/xenon
if ! id xenon >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/xenon/panel --create-home \
    --shell /usr/sbin/nologin xenon
fi
if ! id xenon-agent >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/xenon/agent --create-home \
    --shell /usr/sbin/nologin xenon-agent
fi

install -o root -g root -m 0755 "$panel_binary" /usr/local/bin/xenon
install -o root -g root -m 0755 "$agent_binary" /usr/local/bin/xenon-agent
install -d -o root -g xenon -m 0750 /etc/xenon
install -d -o xenon -g xenon -m 0750 /var/lib/xenon/panel
install -d -o xenon-agent -g xenon-agent -m 0700 /var/lib/xenon/agent

if [ ! -e /etc/xenon/xenon.toml ]; then
  cat > /etc/xenon/xenon.toml <<'EOF'
grpc_addr = "127.0.0.1:50051"
http_addr = "127.0.0.1:18181"
database_path = "/var/lib/xenon/panel/xenon.db"

[subscription_http]
tls_enabled = false
public_base_url = ""
requests_per_minute_per_ip = 120
requests_per_minute_per_token = 60

[backup]
enabled = true
directory = "/var/lib/xenon/panel/backups"
interval_hours = 24
retain_count = 3

[tls]
enabled = false

[registration]
allow_insecure_dev_token = true

[enrollment]
enabled = false

[agent_install]
enabled = false
EOF
  chown root:xenon /etc/xenon/xenon.toml
  chmod 0640 /etc/xenon/xenon.toml
fi

if [ ! -e /var/lib/xenon/agent/agent.toml ]; then
  cat > /var/lib/xenon/agent/agent.toml <<'EOF'
panel_endpoint = "http://127.0.0.1:50051"
agent_id = "xenon-local-test-agent"
node_id = "xenon-local-test-node"
registration_token = "development-only"
interval_seconds = 5

[tls]
enabled = false

[xray]
api_endpoint = "http://127.0.0.1:10085"
inbound_tag = "vless-in"
listen_address = "127.0.0.1"
listen_port = 18443
protocol = "vless"
transport = "tcp"
security = "none"

[spool]
path = "/var/lib/xenon/agent/traffic-spool.json"
max_batches = 256
max_bytes = 4194304
EOF
  chown xenon-agent:xenon-agent /var/lib/xenon/agent/agent.toml
  chmod 0600 /var/lib/xenon/agent/agent.toml
fi

install -o root -g root -m 0644 \
  "$bundle_dir/systemd/xenon.service" /etc/systemd/system/xenon.service
install -o root -g root -m 0644 \
  "$bundle_dir/systemd/xenon-agent.service" /etc/systemd/system/xenon-agent.service

systemctl daemon-reload
systemctl enable --now xenon.service

for _ in $(seq 1 50); do
  if curl --fail --silent http://127.0.0.1:18181/healthz | grep -q '^ok$'; then
    break
  fi
  systemctl is-active --quiet xenon.service || \
    fail "Xenon stopped; inspect journalctl -u xenon"
  sleep 0.2
done
curl --fail --silent http://127.0.0.1:18181/healthz | grep -q '^ok$' || \
  fail "Xenon health check timed out"

systemctl enable --now xenon-agent.service
for _ in $(seq 1 50); do
  systemctl is-active --quiet xenon-agent.service && break
  sleep 0.2
done
systemctl is-active --quiet xenon-agent.service || \
  fail "Agent stopped; inspect journalctl -u xenon-agent"

for _ in $(seq 1 50); do
  if port_in_use 10085; then
    break
  fi
  systemctl is-active --quiet xenon-agent.service || \
    fail "Agent stopped before embedded Xray became ready; inspect journalctl -u xenon-agent"
  sleep 0.2
done
port_in_use 10085 || \
  fail "embedded Xray API did not become ready; inspect journalctl -u xenon-agent"

printf 'Xenon local test installation completed.\n'
printf 'Health: http://127.0.0.1:18181/healthz\n'
printf 'Logs: journalctl -u xenon -u xenon-agent -f\n'
printf 'TUI: systemctl stop xenon && sudo -u xenon XENON_CONFIG=/etc/xenon/xenon.toml /usr/local/bin/xenon\n'
printf 'Restart headless mode after leaving TUI: systemctl start xenon\n'
