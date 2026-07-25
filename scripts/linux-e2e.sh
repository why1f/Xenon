#!/usr/bin/env bash
set -Eeuo pipefail

fail() {
  printf 'linux-e2e: %s\n' "$1" >&2
  exit 1
}

[ "$(uname -s)" = "Linux" ] || fail "Linux is required"
command -v cargo >/dev/null 2>&1 || fail "cargo is required"
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
command -v pgrep >/dev/null 2>&1 || fail "pgrep is required"

: "${XRAY_BINARY_PATH:?set XRAY_BINARY_PATH to Xray-core v26.6.27}"
: "${XRAY_BINARY_VERSION:?set XRAY_BINARY_VERSION to 26.6.27}"
: "${XRAY_BINARY_SHA256:?set XRAY_BINARY_SHA256 to the lowercase binary SHA-256}"

[ "$XRAY_BINARY_VERSION" = "26.6.27" ] || fail "only Xray-core 26.6.27 is supported"
[ -f "$XRAY_BINARY_PATH" ] || fail "XRAY_BINARY_PATH is not a regular file"
printf '%s' "$XRAY_BINARY_SHA256" | grep -Eq '^[0-9a-f]{64}$' || \
  fail "XRAY_BINARY_SHA256 must be 64 lowercase hex characters"
actual_hash="$(sha256sum "$XRAY_BINARY_PATH" | awk '{print $1}')"
[ "$actual_hash" = "$XRAY_BINARY_SHA256" ] || fail "Xray binary SHA-256 mismatch"
XRAY_BINARY_PATH="$(readlink -f "$XRAY_BINARY_PATH")"
export XRAY_BINARY_PATH XRAY_BINARY_VERSION XRAY_BINARY_SHA256

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
panel_pid=""
agent_pid=""

cleanup() {
  if [ -n "$agent_pid" ]; then
    kill "$agent_pid" >/dev/null 2>&1 || true
    wait "$agent_pid" >/dev/null 2>&1 || true
  fi
  if [ -n "$panel_pid" ]; then
    kill "$panel_pid" >/dev/null 2>&1 || true
    wait "$panel_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

cd "$root"
cargo build --release -p xenon -p xenon-agent
version_info="$("$root/target/release/xenon-agent" version-info)"
printf '%s\n' "$version_info" | grep -Fxq 'embedded_xray_version=26.6.27' || \
  fail "release Agent reports the wrong embedded Xray version"
printf '%s\n' "$version_info" | grep -Fxq 'embedded_xray_available=true' || \
  fail "release Agent does not contain an embedded Xray binary"
printf '%s\n' "$version_info" | grep -Fxq "embedded_xray_sha256=$XRAY_BINARY_SHA256" || \
  fail "release Agent reports the wrong embedded Xray SHA-256"

cat > "$work/xenon.toml" <<EOF
grpc_addr = "127.0.0.1:15051"
http_addr = "127.0.0.1:18091"
database_path = "$work/xenon.db"

[subscription_http]
tls_enabled = false
public_base_url = ""
requests_per_minute_per_ip = 120
requests_per_minute_per_token = 60

[registration]
allow_insecure_dev_token = true
EOF

XENON_CONFIG="$work/xenon.toml" \
  "$root/target/release/xenon" --headless >"$work/xenon.log" 2>&1 &
panel_pid=$!
for _ in $(seq 1 100); do
  if curl --fail --silent http://127.0.0.1:18091/healthz >/dev/null; then
    break
  fi
  kill -0 "$panel_pid" >/dev/null 2>&1 || {
    cat "$work/xenon.log" >&2
    fail "Panel exited before becoming healthy"
  }
  sleep 0.1
done
curl --fail --silent http://127.0.0.1:18091/healthz >/dev/null || \
  fail "Panel health check timed out"

cat > "$work/agent.toml" <<EOF
panel_endpoint = "http://127.0.0.1:15051"
agent_id = "linux-e2e-agent"
node_id = "linux-e2e-node"
registration_token = "development-only"
interval_seconds = 1

[tls]
enabled = false

[xray]
api_endpoint = "http://127.0.0.1:11085"
inbound_tag = "vless-in"
listen_address = "127.0.0.1"
listen_port = 18443
protocol = "vless"
transport = "tcp"
security = "none"

[spool]
path = "$work/traffic-spool.json"
max_batches = 128
max_bytes = 1048576
EOF

AGENT_CONFIG="$work/agent.toml" \
  "$root/target/release/xenon-agent" >"$work/agent.log" 2>&1 &
agent_pid=$!

find_xray_child() {
  while read -r candidate; do
    target="$(readlink "/proc/$candidate/exe" 2>/dev/null || true)"
    case "$target" in
      *memfd:xray-core*) printf '%s\n' "$candidate"; return 0 ;;
    esac
  done < <(pgrep -P "$agent_pid" 2>/dev/null || true)
  return 0
}

xray_pid=""
for _ in $(seq 1 100); do
  xray_pid="$(find_xray_child | head -n 1)"
  [ -n "$xray_pid" ] && break
  kill -0 "$agent_pid" >/dev/null 2>&1 || {
    cat "$work/agent.log" >&2
    fail "Agent exited before starting Xray"
  }
  sleep 0.1
done
[ -n "$xray_pid" ] || {
  cat "$work/agent.log" >&2
  fail "embedded Xray memfd child was not found"
}

exe_target="$(readlink "/proc/$xray_pid/exe")"
case "$exe_target" in
  *memfd:xray-core*) ;;
  *) fail "Xray executable is not a memfd: $exe_target" ;;
esac

for interface_path in /sys/class/net/*; do
  interface="$(basename "$interface_path")"
  sys_rx="$(cat "$interface_path/statistics/rx_bytes")"
  sys_tx="$(cat "$interface_path/statistics/tx_bytes")"
  proc_pair="$(awk -F '[: ]+' -v name="$interface" \
    '$2 == name {print $3 " " $11}' /proc/net/dev)"
  [ "$proc_pair" = "$sys_rx $sys_tx" ] || \
    fail "counter mismatch for $interface: proc=$proc_pair sysfs=$sys_rx $sys_tx"
done

kill -KILL "$xray_pid"
replacement_pid=""
for _ in $(seq 1 50); do
  replacement_pid="$(find_xray_child | head -n 1)"
  if [ -n "$replacement_pid" ] && [ "$replacement_pid" != "$xray_pid" ]; then
    break
  fi
  sleep 0.1
done
[ -n "$replacement_pid" ] && [ "$replacement_pid" != "$xray_pid" ] || {
  cat "$work/agent.log" >&2
  fail "Agent did not restart Xray within five seconds"
}

rss_kib="$(awk '/^VmRSS:/ {print $2}' "/proc/$agent_pid/status")"
agent_exe_size="$(stat -c '%s' "$root/target/release/xenon-agent")"
panel_exe_size="$(stat -c '%s' "$root/target/release/xenon")"

kill "$agent_pid"
wait "$agent_pid" >/dev/null 2>&1 || true
agent_pid=""
for _ in $(seq 1 30); do
  [ ! -e "/proc/$replacement_pid" ] && break
  sleep 0.1
done
[ ! -e "/proc/$replacement_pid" ] || fail "Xray survived Agent termination"

printf 'linux-e2e: ok\n'
printf 'xray_version=%s\n' "$XRAY_BINARY_VERSION"
printf 'xray_sha256=%s\n' "$XRAY_BINARY_SHA256"
printf 'agent_size_bytes=%s\n' "$agent_exe_size"
printf 'panel_size_bytes=%s\n' "$panel_exe_size"
printf 'agent_idle_rss_kib=%s\n' "$rss_kib"
