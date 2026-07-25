use std::time::{SystemTime, UNIX_EPOCH};
use xenon_protocol::panel_agent::{
    AgentToPanel, InterfaceSnapshot, InterfaceSnapshotBatch, SystemSnapshot,
};

pub fn heartbeat(agent_id: &str, node_id: &str, uptime_seconds: u64) -> AgentToPanel {
    AgentToPanel {
        payload: Some(
            xenon_protocol::panel_agent::agent_to_panel::Payload::Heartbeat(
                xenon_protocol::panel_agent::Heartbeat {
                    agent_id: agent_id.into(),
                    node_id: node_id.into(),
                    sent_at_unix: now_unix(),
                    uptime_seconds,
                    desired_revision: 0,
                    applied_revision: 0,
                    xray_running: false,
                    xray_restart_count: 0,
                },
            ),
        ),
    }
}

pub fn interface_snapshot(agent_id: &str, node_id: &str, sequence: u64) -> AgentToPanel {
    let interfaces = read_proc_net_dev()
        .into_iter()
        .filter(|(name, _, _)| name != "lo")
        .map(|(name, rx_bytes, tx_bytes)| InterfaceSnapshot {
            name,
            rx_bytes,
            tx_bytes,
        })
        .collect();
    AgentToPanel {
        payload: Some(
            xenon_protocol::panel_agent::agent_to_panel::Payload::Interfaces(
                InterfaceSnapshotBatch {
                    agent_id: agent_id.into(),
                    node_id: node_id.into(),
                    boot_id: read_boot_id(),
                    sequence,
                    sampled_at_unix: now_unix(),
                    interfaces,
                },
            ),
        ),
    }
}

#[derive(Debug, Default)]
pub struct SystemCollector {
    previous_cpu: Option<(u64, u64)>,
}

impl SystemCollector {
    pub fn snapshot(&mut self, agent_id: &str, node_id: &str, sequence: u64) -> AgentToPanel {
        let cpu_usage_basis_points = read_cpu_times()
            .and_then(|current| {
                let previous = self.previous_cpu.replace(current)?;
                let total = current.0.checked_sub(previous.0)?;
                let idle = current.1.checked_sub(previous.1)?;
                (total > 0).then(|| {
                    total
                        .saturating_sub(idle)
                        .saturating_mul(10_000)
                        .checked_div(total)
                        .unwrap_or_default()
                        .min(10_000) as u32
                })
            })
            .unwrap_or_default();
        if self.previous_cpu.is_none() {
            self.previous_cpu = read_cpu_times();
        }
        let (load_1_milli, load_5_milli, load_15_milli) = read_load_average();
        let (memory_total_bytes, memory_used_bytes) = read_memory();
        let (disk_total_bytes, disk_used_bytes) = read_disk_usage();
        AgentToPanel {
            payload: Some(
                xenon_protocol::panel_agent::agent_to_panel::Payload::System(SystemSnapshot {
                    agent_id: agent_id.into(),
                    node_id: node_id.into(),
                    sequence,
                    sampled_at_unix: now_unix(),
                    cpu_usage_basis_points,
                    load_1_milli,
                    load_5_milli,
                    load_15_milli,
                    memory_total_bytes,
                    memory_used_bytes,
                    disk_total_bytes,
                    disk_used_bytes,
                }),
            ),
        }
    }
}

fn read_boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn read_proc_net_dev() -> Vec<(String, u64, u64)> {
    std::fs::read_to_string("/proc/net/dev")
        .ok()
        .map(|content| parse_proc_net_dev(&content))
        .unwrap_or_default()
}

fn parse_proc_net_dev(content: &str) -> Vec<(String, u64, u64)> {
    content
        .lines()
        .skip(2)
        .filter_map(|line| {
            let (name, counters) = line.split_once(':')?;
            let values = counters
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if values.len() < 9 {
                return None;
            }
            Some((name.trim().to_string(), values[0], values[8]))
        })
        .collect()
}

fn read_cpu_times() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    parse_cpu_times(&content)
}

fn parse_cpu_times(content: &str) -> Option<(u64, u64)> {
    let line = content.lines().find(|line| line.starts_with("cpu "))?;
    let values = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let total = values.iter().copied().fold(0_u64, u64::saturating_add);
    let idle = values[3].saturating_add(values.get(4).copied().unwrap_or_default());
    Some((total, idle))
}

fn read_load_average() -> (u64, u64, u64) {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .map(|content| parse_load_average(&content))
        .unwrap_or_default()
}

fn parse_load_average(content: &str) -> (u64, u64, u64) {
    let mut values = content
        .split_whitespace()
        .take(3)
        .map(|value| value.parse::<f64>().unwrap_or_default());
    let milli = |value: f64| (value.max(0.0) * 1000.0).round() as u64;
    (
        milli(values.next().unwrap_or_default()),
        milli(values.next().unwrap_or_default()),
        milli(values.next().unwrap_or_default()),
    )
}

fn read_memory() -> (u64, u64) {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|content| parse_memory(&content))
        .unwrap_or_default()
}

fn parse_memory(content: &str) -> (u64, u64) {
    let value = |name: &str| {
        content.lines().find_map(|line| {
            let (key, rest) = line.split_once(':')?;
            (key == name)
                .then(|| rest.split_whitespace().next()?.parse::<u64>().ok())
                .flatten()
        })
    };
    let total = value("MemTotal").unwrap_or_default().saturating_mul(1024);
    let available = value("MemAvailable")
        .unwrap_or_default()
        .saturating_mul(1024);
    (total, total.saturating_sub(available))
}

#[cfg(target_family = "unix")]
fn read_disk_usage() -> (u64, u64) {
    let path = std::ffi::CString::new("/").expect("static path");
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL terminated and stats points to writable storage.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return (0, 0);
    }
    // SAFETY: statvfs returned success and initialized stats.
    let stats = unsafe { stats.assume_init() };
    let fragment_size = stats.f_frsize;
    let total = stats.f_blocks.saturating_mul(fragment_size);
    let available = stats.f_bavail.saturating_mul(fragment_size);
    (total, total.saturating_sub(available))
}

#[cfg(not(target_family = "unix"))]
fn read_disk_usage() -> (u64, u64) {
    (0, 0)
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{parse_cpu_times, parse_load_average, parse_memory, parse_proc_net_dev};

    #[test]
    fn parses_linux_interface_counters() {
        let input = "Inter-| Receive | Transmit\n \
                     face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n                     eth0: 12 2 0 0 0 0 0 0 34 4 0 0 0 0 0 0 0\n                     lo: 1 1 0 0 0 0 0 0 2 1 0 0 0 0 0 0 0\n";
        assert_eq!(
            parse_proc_net_dev(input),
            vec![("eth0".into(), 12, 34), ("lo".into(), 1, 2)]
        );
    }

    #[test]
    fn parses_linux_system_metrics() {
        assert_eq!(parse_cpu_times("cpu  10 2 3 20 5 0 0 0\n"), Some((40, 25)));
        assert_eq!(
            parse_load_average("1.25 0.50 0.10 1/100 1\n"),
            (1250, 500, 100)
        );
        assert_eq!(
            parse_memory("MemTotal: 1000 kB\nMemAvailable: 250 kB\n"),
            (1_024_000, 768_000)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_proc_interface_counters_match_sysfs_snapshot() {
        let proc_values =
            parse_proc_net_dev(&std::fs::read_to_string("/proc/net/dev").expect("/proc/net/dev"));
        let (name, proc_rx, proc_tx) = proc_values
            .into_iter()
            .find(|(name, _, _)| name != "lo")
            .expect("non-loopback interface");
        let sysfs = |direction: &str| {
            std::fs::read_to_string(format!(
                "/sys/class/net/{name}/statistics/{direction}_bytes"
            ))
            .expect("sysfs interface counter")
            .trim()
            .parse::<u64>()
            .expect("sysfs counter value")
        };
        let sys_rx = sysfs("rx");
        let sys_tx = sysfs("tx");
        assert!(sys_rx >= proc_rx);
        assert!(sys_tx >= proc_tx);
        assert!(sys_rx - proc_rx < 1024 * 1024);
        assert!(sys_tx - proc_tx < 1024 * 1024);
    }
}
