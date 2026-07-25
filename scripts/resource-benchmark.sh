#!/usr/bin/env bash
set -Eeuo pipefail

fail() {
  printf 'resource-benchmark: %s\n' "$1" >&2
  exit 1
}

[ "$(uname -s)" = "Linux" ] || fail "Linux is required"
: "${AGENT_PID:?set AGENT_PID to a running xenon-agent process}"
printf '%s' "$AGENT_PID" | grep -Eq '^[1-9][0-9]*$' || fail "AGENT_PID is invalid"
[ -r "/proc/$AGENT_PID/stat" ] || fail "Agent process does not exist"

executable="$(readlink -f "/proc/$AGENT_PID/exe")"
case "$(basename "$executable")" in
  xenon-agent) ;;
  *) fail "AGENT_PID does not point to xenon-agent" ;;
esac

duration="${SAMPLE_SECONDS:-10}"
printf '%s' "$duration" | grep -Eq '^[1-9][0-9]*$' || fail "SAMPLE_SECONDS is invalid"

clock_ticks="$(getconf CLK_TCK)"
start_ticks="$(awk '{print $14 + $15}' "/proc/$AGENT_PID/stat")"
start_time="$(date +%s%N)"
sleep "$duration"
[ -r "/proc/$AGENT_PID/stat" ] || fail "Agent exited during sampling"
end_ticks="$(awk '{print $14 + $15}' "/proc/$AGENT_PID/stat")"
end_time="$(date +%s%N)"

elapsed_ns=$((end_time - start_time))
cpu_ticks=$((end_ticks - start_ticks))
cpu_basis_points=$((cpu_ticks * 10000000000000 / clock_ticks / elapsed_ns))
rss_kib="$(awk '/^VmRSS:/ {print $2}' "/proc/$AGENT_PID/status")"
peak_rss_kib="$(awk '/^VmHWM:/ {print $2}' "/proc/$AGENT_PID/status")"
binary_size="$(stat -c '%s' "$executable")"
xray_pid="$(pgrep -P "$AGENT_PID" | head -n 1 || true)"
xray_rss_kib=0
if [ -n "$xray_pid" ] && [ -r "/proc/$xray_pid/status" ]; then
  xray_rss_kib="$(awk '/^VmRSS:/ {print $2}' "/proc/$xray_pid/status")"
fi

printf 'agent_binary=%s\n' "$executable"
printf 'agent_size_bytes=%s\n' "$binary_size"
printf 'agent_rss_kib=%s\n' "$rss_kib"
printf 'agent_peak_rss_kib=%s\n' "$peak_rss_kib"
printf 'agent_cpu_basis_points=%s\n' "$cpu_basis_points"
printf 'xray_child_rss_kib=%s\n' "$xray_rss_kib"
printf 'sample_seconds=%s\n' "$duration"
