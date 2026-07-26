#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() {
  printf 'install-panel: %s\n' "$1" >&2
  exit 1
}

UNINSTALL=0
PURGE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --uninstall) UNINSTALL=1; shift ;;
    --purge) UNINSTALL=1; PURGE=1; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || fail "run as root"
[ "$(uname -s)" = "Linux" ] || fail "Linux is required"
command -v systemctl >/dev/null 2>&1 || fail "systemd is required"

if [ "$UNINSTALL" -eq 1 ]; then
  systemctl disable --now xenon.service >/dev/null 2>&1 || true
  rm -f /etc/systemd/system/xenon.service /usr/local/bin/xenon /usr/local/bin/xenon-tui
  # A loopback test install also runs a local Agent pointed at this Panel;
  # remove it too so a later production install starts clean.
  if grep -qs 'panel_endpoint = "http://127.0.0.1:50051"' /var/lib/xenon/agent/agent.toml 2>/dev/null || \
     grep -qs 'panel_endpoint = "https://127.0.0.1:50051"' /var/lib/xenon/agent/agent.toml 2>/dev/null; then
    printf 'Removing local loopback test Agent as well.\n'
    systemctl disable --now xenon-agent.service >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/xenon-agent.service /usr/local/bin/xenon-agent
    rm -rf /var/lib/xenon/agent
  fi
  systemctl daemon-reload
  if [ "$PURGE" -eq 1 ]; then
    rm -rf /etc/xenon /var/lib/xenon
    printf 'Xenon Panel uninstalled; configuration, certificates, and data removed.\n'
  else
    printf 'Xenon Panel uninstalled; /etc/xenon and /var/lib/xenon were kept.\n'
    printf 'Remove them too with: --purge\n'
  fi
  exit 0
fi

for command_name in curl sha256sum tar openssl awk sort sed grep cut paste; do
  command -v "$command_name" >/dev/null 2>&1 || fail "$command_name is required"
done

if [ -f /etc/xenon/xenon.toml ] && \
  grep -q 'allow_insecure_dev_token = true' /etc/xenon/xenon.toml; then
  fail "found a loopback test configuration at /etc/xenon/xenon.toml; \
uninstall the test environment first: re-run this script with --uninstall --purge"
fi

case "$(uname -m)" in
  x86_64 | amd64) architecture="x86_64" ;;
  aarch64 | arm64) architecture="aarch64" ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

repository="${XENON_REPOSITORY:-why1f/Xenon}"
version="${XENON_VERSION:-}"
if [ -z "$version" ]; then
  # GitHub's release array is not reliably ordered for numeric prerelease
  # suffixes (alpha.10 may appear after alpha.9). Select by publication time.
  release_json="$(curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
    --header 'Accept: application/vnd.github+json' \
    --header 'X-GitHub-Api-Version: 2022-11-28' \
    "https://api.github.com/repos/${repository}/releases?per_page=100")" || true
  version="$(paste \
    <(printf '%s' "$release_json" | grep -o '"tag_name":[[:space:]]*"[^"]*"' | cut -d '"' -f 4) \
    <(printf '%s' "$release_json" | grep -o '"published_at":[[:space:]]*"[^"]*"' | cut -d '"' -f 4) |
    awk -F '\t' '{ print $2 "\t" $1 }' |
    sort -r | sed -n '1{s/^[^\t]*\t//;p;}')" || true
  [ -n "$version" ] || \
    fail "unable to determine the latest release; set XENON_VERSION explicitly"
fi

# The public host agents and subscription clients will reach. A domain is
# preferred; a public IPv4 works and is put into the certificate as an IP SAN.
host="${XENON_HOST:-}"
if [ -z "$host" ]; then
  host="$(curl --fail --silent --max-time 10 -4 https://api.ipify.org || true)"
fi
if [ -z "$host" ]; then
  host="$(hostname -I 2>/dev/null | awk '{print $1}')"
fi
printf '%s' "$host" | grep -Eq '^[A-Za-z0-9.-]{1,253}$' || \
  fail "could not determine a public host; re-run with XENON_HOST=<domain-or-ip>"

release_url="https://github.com/${repository}/releases/download/${version}"
artifact="xenon-linux-${architecture}"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

printf 'Downloading Xenon %s for %s...\n' "$version" "$architecture"
fetch() {
  curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
    "$release_url/$1" --output "$work_dir/$1"
}
fetch "${artifact}.tar.gz"
fetch "${artifact}.tar.gz.sha256"
fetch "xenon-agent-linux-x86_64.sha256"
fetch "xenon-agent-linux-aarch64.sha256"
(cd "$work_dir" && sha256sum --check --strict "${artifact}.tar.gz.sha256" >/dev/null)
tar -C "$work_dir" -xzf "$work_dir/${artifact}.tar.gz"
bundle_dir="$work_dir/$artifact"
[ -x "$bundle_dir/bin/xenon" ] || fail "bundle is missing bin/xenon"

agent_sha_x86_64="$(awk '{print $1}' "$work_dir/xenon-agent-linux-x86_64.sha256")"
agent_sha_aarch64="$(awk '{print $1}' "$work_dir/xenon-agent-linux-aarch64.sha256")"
printf '%s' "$agent_sha_x86_64" | grep -Eq '^[0-9a-f]{64}$' || fail "bad x86_64 agent digest"
printf '%s' "$agent_sha_aarch64" | grep -Eq '^[0-9a-f]{64}$' || fail "bad aarch64 agent digest"
agent_version="${version#v}"

if ! id xenon >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/xenon/panel --create-home \
    --shell /usr/sbin/nologin xenon
fi

systemctl stop xenon.service >/dev/null 2>&1 || true
install -o root -g root -m 0755 "$bundle_dir/bin/xenon" /usr/local/bin/xenon
if [ -f "$bundle_dir/scripts/xenon-tui.sh" ]; then
  install -o root -g root -m 0755 "$bundle_dir/scripts/xenon-tui.sh" \
    /usr/local/bin/xenon-tui
fi
install -d -o root -g xenon -m 0750 /etc/xenon
install -d -o root -g xenon -m 0750 /etc/xenon/tls
install -d -o xenon -g xenon -m 0750 /var/lib/xenon/panel
install -d -o xenon -g xenon -m 0750 /var/lib/xenon/panel/backups

tls_dir=/etc/xenon/tls
if [ ! -f "$tls_dir/server-ca.crt" ]; then
  printf 'Generating Panel certificates for %s...\n' "$host"
  if printf '%s' "$host" | grep -Eq '^[0-9.]+$'; then
    san="IP:$host"
  else
    san="DNS:$host"
  fi
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$tls_dir/server-ca.key" -out "$tls_dir/server-ca.crt" \
    -subj "/CN=Xenon Server CA" -days 3650 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$tls_dir/server.key" -out "$work_dir/server.csr" \
    -subj "/CN=$host" 2>/dev/null
  openssl x509 -req -in "$work_dir/server.csr" \
    -CA "$tls_dir/server-ca.crt" -CAkey "$tls_dir/server-ca.key" \
    -CAcreateserial -out "$tls_dir/server.crt" -days 825 \
    -extfile <(printf 'subjectAltName=%s\nextendedKeyUsage=serverAuth\n' "$san") \
    2>/dev/null
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$tls_dir/clients-ca.key" -out "$tls_dir/clients-ca.crt" \
    -subj "/CN=Xenon Agents CA" -days 3650 2>/dev/null
  chown root:xenon "$tls_dir"/*
  chmod 0644 "$tls_dir"/*.crt
  chmod 0640 "$tls_dir/server.key"
  chown root:root "$tls_dir/server-ca.key"
  chmod 0600 "$tls_dir/server-ca.key"
fi

# Enrollment signs Agent certificates at runtime. Its CA key must be readable by
# the xenon service account while remaining inaccessible to group/other users.
if [ -f "$tls_dir/clients-ca.key" ]; then
  chown xenon:xenon "$tls_dir/clients-ca.key"
  chmod 0600 "$tls_dir/clients-ca.key"
fi

if [ ! -f /etc/xenon/xenon.toml ]; then
  cat > /etc/xenon/xenon.toml <<EOF
grpc_addr = "0.0.0.0:50051"
http_addr = "0.0.0.0:18181"
database_path = "/var/lib/xenon/panel/xenon.db"

[subscription_http]
tls_enabled = false
allow_public_plaintext = true
public_base_url = "http://${host}:18181"
requests_per_minute_per_ip = 120
requests_per_minute_per_token = 60

[traffic_retention]
maintenance_interval_seconds = 3600
raw_event_days = 30
interface_snapshot_days = 30
system_snapshot_days = 7
hourly_aggregate_days = 0
daily_aggregate_days = 0

[backup]
enabled = true
directory = "/var/lib/xenon/panel/backups"
interval_hours = 24
retain_count = 7

[tls]
enabled = true
cert_path = "/etc/xenon/tls/server.crt"
key_path = "/etc/xenon/tls/server.key"
client_ca_path = "/etc/xenon/tls/clients-ca.crt"

[registration]
allow_insecure_dev_token = false

[enrollment]
enabled = true
addr = "0.0.0.0:50052"
ca_cert_path = "/etc/xenon/tls/clients-ca.crt"
ca_key_path = "/etc/xenon/tls/clients-ca.key"
certificate_valid_days = 90

[agent_install]
enabled = true
script_url = "https://raw.githubusercontent.com/${repository}/main/scripts/install-agent.sh"
binary_url = "${release_url}/xenon-agent-linux-{arch}"
binary_sha256_x86_64 = "${agent_sha_x86_64}"
binary_sha256_aarch64 = "${agent_sha_aarch64}"
binary_version = "${agent_version}"
ca_path = "/etc/xenon/tls/server-ca.crt"
panel_endpoint = "https://${host}:50051"
enrollment_endpoint = "https://${host}:50052"
server_name = "${host}"
EOF
  chown root:xenon /etc/xenon/xenon.toml
  chmod 0640 /etc/xenon/xenon.toml
else
  if grep -Fxq 'http_addr = "0.0.0.0:18181"' /etc/xenon/xenon.toml && \
    awk '
      /^\[/ { in_subscription = ($0 == "[subscription_http]") }
      in_subscription && $0 == "tls_enabled = false" { plaintext = 1 }
      in_subscription && $0 ~ /^public_base_url = "http:\/\/[^"]+:18181"$/ { public_url = 1 }
      END { exit !(plaintext && public_url) }
    ' /etc/xenon/xenon.toml && \
    ! awk '
      /^\[/ { in_subscription = ($0 == "[subscription_http]") }
      in_subscription && $0 ~ /^allow_public_plaintext[[:space:]]*=/ { found = 1 }
      END { exit !found }
    ' /etc/xenon/xenon.toml; then
    config_tmp="$(mktemp /etc/xenon/xenon.toml.tmp.XXXXXX)"
    awk '
      /^\[/ { in_subscription = ($0 == "[subscription_http]") }
      in_subscription && $0 == "tls_enabled = false" {
        print
        print "allow_public_plaintext = true"
        next
      }
      { print }
    ' /etc/xenon/xenon.toml > "$config_tmp"
    chown root:xenon "$config_tmp"
    chmod 0640 "$config_tmp"
    mv "$config_tmp" /etc/xenon/xenon.toml
    printf 'Migrated the legacy installer subscription HTTP configuration.\n'
  else
    printf 'Keeping existing /etc/xenon/xenon.toml.\n'
  fi
fi

install -o root -g root -m 0644 \
  "$bundle_dir/systemd/xenon.service" /etc/systemd/system/xenon.service
systemctl daemon-reload
systemctl enable --now xenon.service
stable=0
for _ in $(seq 1 30); do
  if systemctl is-active --quiet xenon.service; then
    stable=$((stable + 1))
    [ "$stable" -ge 5 ] && break
  else
    stable=0
  fi
  sleep 0.2
done
[ "$stable" -ge 5 ] || \
  fail "xenon.service did not start; inspect journalctl -u xenon"

printf '\nXenon Panel %s installed for host %s.\n' "$version" "$host"
printf 'Open the TUI:            sudo xenon-tui\n'
printf 'Add a managed host:      press [a] on the Host page, then run the printed\n'
printf '                         one-line install command on the target Linux VPS.\n'
printf 'Required open ports:     50051 (gRPC mTLS), 50052 (enrollment), 18181 (subscriptions)\n'
printf 'Config:                  /etc/xenon/xenon.toml\n'
printf 'Logs:                    journalctl -u xenon -f\n'
