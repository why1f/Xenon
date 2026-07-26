#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'xenon-tui: %s\n' "$1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail "run as root: sudo xenon-tui"
[ -t 0 ] && [ -t 1 ] || fail "a terminal is required"
command -v systemctl >/dev/null 2>&1 || fail "systemd is required"
[ -f /etc/xenon/xenon.toml ] || fail "/etc/xenon/xenon.toml not found"
[ -x /usr/local/bin/xenon ] || fail "/usr/local/bin/xenon not found"

# The TUI runs in the same process as the Panel services, so the headless
# service must yield its ports first and take over again when the TUI exits.
restart_after=0
if systemctl is-active --quiet xenon.service || \
  systemctl is-enabled --quiet xenon.service; then
  restart_after=1
fi
# Stop unconditionally so an activating/restarting service cannot race the TUI
# for the database lock or listening ports.
systemctl stop xenon.service
restore() {
  if [ "$restart_after" -eq 1 ]; then
    systemctl start xenon.service || \
      printf 'xenon-tui: failed to restart xenon.service; run: systemctl start xenon\n' >&2
  fi
}
trap restore EXIT

sudo -u xenon XENON_CONFIG=/etc/xenon/xenon.toml /usr/local/bin/xenon
