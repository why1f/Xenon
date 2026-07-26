use crate::{config::AgentInstallConfig, secrets::sha256_hex, RuntimeState};
use anyhow::Context;
use chrono::Utc;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, Tabs, Wrap},
    Terminal,
};
use std::{collections::HashMap, io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, RwLock};
use uuid::Uuid;
use xenon_storage::{models, Database};

#[derive(Debug, Clone, Default)]
struct TuiSnapshot {
    connected_agents: usize,
    last_agent_event: Option<String>,
    users: Vec<models::UserSummary>,
    nodes: Vec<models::NodeOverview>,
    proxy_nodes: Vec<models::ProxyNodeRecord>,
    interfaces: Vec<models::InterfaceRecord>,
    agent_events: Vec<String>,
    user_detail: Option<models::UserDetail>,
    notice: String,
    down_rate_bps: u64,
    up_rate_bps: u64,
    down_rate_history: Vec<u64>,
    up_rate_history: Vec<u64>,
    host_nic: Vec<HostNicSnapshot>,
}

#[derive(Debug, Clone)]
struct HostNicSnapshot {
    node_id: String,
    rx_bytes: i64,
    tx_bytes: i64,
    down_rate_bps: u64,
    up_rate_bps: u64,
    sampled_at: i64,
}

#[derive(Debug, Default)]
struct NicRateTracker {
    last_sample: Option<(i64, i64, i64)>,
    down_rate_bps: u64,
    up_rate_bps: u64,
    down_history: std::collections::VecDeque<u64>,
    up_history: std::collections::VecDeque<u64>,
}

impl NicRateTracker {
    const HISTORY_CAPACITY: usize = 240;

    fn observe(&mut self, totals: Option<models::NicCounterTotals>) {
        if let Some(totals) = totals {
            match self.last_sample {
                Some((sampled_at, rx, tx)) if totals.sampled_at > sampled_at => {
                    let elapsed = (totals.sampled_at - sampled_at) as u64;
                    let delta_rx = totals.rx_bytes - rx;
                    let delta_tx = totals.tx_bytes - tx;
                    // Negative deltas mean the absolute counters were reset
                    // (agent reboot / new boot_id); re-baseline instead of
                    // rendering a bogus spike.
                    if delta_rx >= 0 && delta_tx >= 0 && elapsed > 0 {
                        self.down_rate_bps = delta_rx as u64 / elapsed;
                        self.up_rate_bps = delta_tx as u64 / elapsed;
                    } else {
                        self.down_rate_bps = 0;
                        self.up_rate_bps = 0;
                    }
                    self.last_sample = Some((totals.sampled_at, totals.rx_bytes, totals.tx_bytes));
                }
                Some(_) => {}
                None => {
                    self.last_sample = Some((totals.sampled_at, totals.rx_bytes, totals.tx_bytes));
                }
            }
        } else {
            self.down_rate_bps = 0;
            self.up_rate_bps = 0;
        }
        self.down_history.push_back(self.down_rate_bps);
        self.up_history.push_back(self.up_rate_bps);
        while self.down_history.len() > Self::HISTORY_CAPACITY {
            self.down_history.pop_front();
        }
        while self.up_history.len() > Self::HISTORY_CAPACITY {
            self.up_history.pop_front();
        }
    }

    fn apply(&self, snapshot: &mut TuiSnapshot) {
        snapshot.down_rate_bps = self.down_rate_bps;
        snapshot.up_rate_bps = self.up_rate_bps;
        snapshot.down_rate_history = self.down_history.iter().copied().collect();
        snapshot.up_rate_history = self.up_history.iter().copied().collect();
    }
}

fn apply_host_nic_rates(snapshot: &mut TuiSnapshot, trackers: &HashMap<String, NicRateTracker>) {
    snapshot.host_nic = snapshot
        .nodes
        .iter()
        .filter_map(|node| {
            let tracker = trackers.get(&node.id)?;
            let (sampled_at, rx_bytes, tx_bytes) = tracker.last_sample?;
            Some(HostNicSnapshot {
                node_id: node.id.clone(),
                rx_bytes,
                tx_bytes,
                down_rate_bps: tracker.down_rate_bps,
                up_rate_bps: tracker.up_rate_bps,
                sampled_at,
            })
        })
        .collect();
}

#[derive(Debug, Clone)]
struct CreateSubscriptionInput {
    username: String,
    name: String,
    node_ids: String,
    limit_bytes: String,
    expires_at: String,
    multiplier: String,
    nic_bindings: String,
    reset_policy: String,
}

#[derive(Debug, Clone)]
struct CreateHostInput {
    name: String,
    landing_host: String,
}

#[derive(Debug, Clone)]
struct ProxyNodeInput {
    host_id: String,
    name: String,
    profile: String,
    listen_port: String,
    publish_host: String,
    publish_port: String,
    server_name: String,
    websocket_path: String,
    vless_encryption: String,
    reality_public_key: String,
    reality_short_id: String,
    reality_fingerprint: String,
}

#[derive(Debug, Clone)]
struct CreateNicBindingInput {
    node_id: String,
    interface_name: String,
    traffic_limit_bytes: String,
    initial_used_bytes: String,
    billing_direction: String,
    reset_policy: String,
}

#[derive(Debug, Clone)]
struct EditSubscriptionInput {
    subscription_id: String,
    starts_at: i64,
    current_cycle_start: i64,
    name: String,
    node_ids: String,
    limit_bytes: String,
    expires_at: String,
    multiplier: String,
    reset_policy: String,
    status: String,
}

enum TuiCommand {
    Refresh,
    CreateSubscription(CreateSubscriptionInput),
    CreateHost(CreateHostInput),
    CreateProxyNode(ProxyNodeInput),
    RevokeNode(String),
    ShowUser(String),
    ResetSubscription {
        user_id: String,
        subscription_id: String,
    },
    AddNicBinding {
        user_id: String,
        subscription_id: String,
        input: CreateNicBindingInput,
    },
    UnbindNicBinding {
        user_id: String,
        binding_id: String,
    },
    ResetNicBinding {
        user_id: String,
        binding_id: String,
    },
    UpdateSubscription {
        user_id: String,
        input: EditSubscriptionInput,
    },
    RotateSubscriptionToken {
        user_id: String,
        subscription_id: String,
    },
    RotateSubscriptionUuid {
        user_id: String,
        subscription_id: String,
    },
    UpdateHost {
        node_id: String,
        input: CreateHostInput,
    },
    SetHostStatus {
        node_id: String,
        status: String,
    },
    UpdateProxyNode {
        proxy_node_id: String,
        input: ProxyNodeInput,
    },
    SetProxyNodeStatus {
        proxy_node_id: String,
        status: String,
    },
    DeleteProxyNode(String),
    ShowAgentInstall(String),
    ShowAgentUpgrade(String),
    DeleteHost(String),
}

pub async fn run(
    state: Arc<RwLock<RuntimeState>>,
    database: Database,
    grpc_addr: String,
    subscription_base_url: String,
    agent_install: AgentInstallConfig,
) -> anyhow::Result<()> {
    let (snapshot_tx, snapshot_rx) = watch::channel(TuiSnapshot::default());
    let (command_tx, mut command_rx) = mpsc::channel::<TuiCommand>(8);
    let mut tui_task = tokio::task::spawn_blocking(move || run_blocking(snapshot_rx, command_tx));
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    let mut user_detail = None;
    let mut notice = String::new();
    let mut rate_tracker = NicRateTracker::default();
    let mut host_rate_trackers = HashMap::<String, NicRateTracker>::new();

    loop {
        tokio::select! {
            result = &mut tui_task => {
                result.context("join TUI task")??;
                break;
            }
            _ = refresh.tick() => {
                rate_tracker.observe(database.latest_nic_totals().await.unwrap_or_default());
                if let Ok(totals) = database.latest_nic_totals_by_node().await {
                    let active = totals.iter().map(|total| total.node_id.as_str()).collect::<std::collections::HashSet<_>>();
                    host_rate_trackers.retain(|node_id, _| active.contains(node_id.as_str()));
                    for total in totals {
                        host_rate_trackers.entry(total.node_id).or_default().observe(Some(models::NicCounterTotals {
                            rx_bytes: total.rx_bytes,
                            tx_bytes: total.tx_bytes,
                            sampled_at: total.sampled_at,
                        }));
                    }
                }
                let mut snapshot = load_snapshot(&state, &database, notice.clone(), user_detail.clone()).await?;
                rate_tracker.apply(&mut snapshot);
                apply_host_nic_rates(&mut snapshot, &host_rate_trackers);
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    break;
                };
                let result = match command {
                    TuiCommand::Refresh => {
                        if let Some(user_id) = user_detail
                            .as_ref()
                            .map(|detail| detail.user.id.clone())
                        {
                            user_detail = database.user_detail(&user_id).await?;
                        }
                        Ok(String::new())
                    }
                    TuiCommand::CreateSubscription(input) => create_subscription(&database, &subscription_base_url, input).await,
                    TuiCommand::CreateHost(input) => create_host(&database, &grpc_addr, &agent_install, input).await,
                    TuiCommand::CreateProxyNode(input) => create_proxy_node(&database, input).await,
                    TuiCommand::RevokeNode(node_id) => revoke_node(&database, &node_id).await,
                    TuiCommand::ShowUser(user_id) => {
                        match database.user_detail(&user_id).await {
                            Ok(Some(detail)) => {
                                user_detail = Some(detail);
                                Ok(format!("opened user {user_id}"))
                            }
                            Ok(None) => Err(anyhow::anyhow!("user no longer exists")),
                            Err(error) => Err(error.into()),
                        }
                    }
                    TuiCommand::ResetSubscription {
                        user_id,
                        subscription_id,
                    } => {
                        match database
                            .reset_subscription_cycle(&subscription_id, Utc::now().timestamp())
                            .await
                        {
                            Ok(true) => match database.user_detail(&user_id).await {
                                Ok(detail) => {
                                    user_detail = detail;
                                    Ok(format!("reset subscription {subscription_id}"))
                                }
                                Err(error) => Err(error.into()),
                            },
                            Ok(false) => Err(anyhow::anyhow!("subscription no longer exists")),
                            Err(error) => Err(error.into()),
                        }
                    }
                    TuiCommand::AddNicBinding {
                        user_id,
                        subscription_id,
                        input,
                    } => apply_user_detail(
                        &database,
                        &user_id,
                        &mut user_detail,
                        add_nic_binding(&database, &subscription_id, input).await,
                    )
                    .await,
                    TuiCommand::UnbindNicBinding { user_id, binding_id } => {
                        let result = async {
                            if database
                                .unbind_nic_binding(&binding_id, Utc::now().timestamp())
                                .await?
                            {
                                Ok(format!("unbound NIC binding {binding_id}"))
                            } else {
                                Err(anyhow::anyhow!("NIC binding no longer exists"))
                            }
                        }
                        .await;
                        apply_user_detail(&database, &user_id, &mut user_detail, result).await
                    }
                    TuiCommand::ResetNicBinding { user_id, binding_id } => {
                        let result = async {
                            if database
                                .reset_nic_binding_cycle(&binding_id, Utc::now().timestamp())
                                .await?
                            {
                                Ok(format!("reset NIC binding {binding_id}"))
                            } else {
                                Err(anyhow::anyhow!("NIC binding no longer exists"))
                            }
                        }
                        .await;
                        apply_user_detail(&database, &user_id, &mut user_detail, result).await
                    }
                    TuiCommand::UpdateSubscription { user_id, input } => {
                        apply_user_detail(
                            &database,
                            &user_id,
                            &mut user_detail,
                            update_subscription(&database, input).await,
                        )
                        .await
                    }
                    TuiCommand::RotateSubscriptionToken {
                        user_id,
                        subscription_id,
                    } => apply_user_detail(
                        &database,
                        &user_id,
                        &mut user_detail,
                        rotate_subscription_token(&database, &subscription_id).await,
                    )
                    .await,
                    TuiCommand::RotateSubscriptionUuid {
                        user_id,
                        subscription_id,
                    } => apply_user_detail(
                        &database,
                        &user_id,
                        &mut user_detail,
                        rotate_subscription_uuid(&database, &subscription_id).await,
                    )
                    .await,
                    TuiCommand::UpdateHost { node_id, input } => {
                        update_host(&database, &node_id, input).await
                    }
                    TuiCommand::SetHostStatus { node_id, status } => {
                        set_host_status(&database, &node_id, &status).await
                    }
                    TuiCommand::UpdateProxyNode { proxy_node_id, input } => {
                        update_proxy_node(&database, &proxy_node_id, input).await
                    }
                    TuiCommand::SetProxyNodeStatus { proxy_node_id, status } => {
                        set_proxy_node_status(&database, &proxy_node_id, &status).await
                    }
                    TuiCommand::DeleteProxyNode(proxy_node_id) => delete_proxy_node(&database, &proxy_node_id).await,
                    TuiCommand::ShowAgentInstall(node_id) => {
                        create_host_registration(&database, &grpc_addr, &agent_install, &node_id).await
                    }
                    TuiCommand::ShowAgentUpgrade(node_id) => {
                        agent_upgrade_command(&agent_install, &node_id)
                    }
                    TuiCommand::DeleteHost(node_id) => delete_host(&database, &node_id).await,
                };
                notice = match result {
                    Ok(message) => message,
                    Err(error) => format!("operation failed: {error}"),
                };
                let snapshot = {
                    let mut snapshot = load_snapshot(&state, &database, notice.clone(), user_detail.clone()).await?;
                    rate_tracker.apply(&mut snapshot);
                    apply_host_nic_rates(&mut snapshot, &host_rate_trackers);
                    snapshot
                };
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn revoke_node(database: &Database, node_id: &str) -> anyhow::Result<String> {
    let node_id = node_id.trim();
    if node_id.is_empty() {
        anyhow::bail!("host ID is required");
    }
    let revoked = database
        .revoke_node_certificates(node_id, Utc::now().timestamp())
        .await?;
    if revoked == 0 {
        anyhow::bail!("host has no active Agent certificate");
    }
    Ok(format!(
        "revoked {revoked} Agent certificate(s) for host {node_id}"
    ))
}

async fn apply_user_detail(
    database: &Database,
    user_id: &str,
    user_detail: &mut Option<models::UserDetail>,
    result: anyhow::Result<String>,
) -> anyhow::Result<String> {
    let message = result?;
    let detail = database
        .user_detail(user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user no longer exists"))?;
    *user_detail = Some(detail);
    Ok(message)
}

async fn add_nic_binding(
    database: &Database,
    subscription_id: &str,
    input: CreateNicBindingInput,
) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let traffic_limit_bytes = input
        .traffic_limit_bytes
        .trim()
        .parse::<i64>()
        .context("invalid NIC limit")?;
    let initial_used_bytes = input
        .initial_used_bytes
        .trim()
        .parse::<i64>()
        .context("invalid NIC initial usage")?;
    let billing_direction = input.billing_direction.trim().to_ascii_lowercase();
    if !matches!(billing_direction.as_str(), "rx_tx" | "tx_only" | "rx_only") {
        anyhow::bail!("NIC direction must be rx_tx, tx_only, or rx_only");
    }
    let policy = xenon_domain::ResetPolicy::parse(&input.reset_policy, now)
        .context("invalid NIC reset policy")?;
    let (reset_policy, reset_anchor) = policy.stored();
    let cycle = policy
        .cycle_at(now, now)
        .context("calculate NIC billing cycle")?;
    database
        .add_nic_binding(
            subscription_id,
            &models::NewNicBinding {
                id: Uuid::now_v7().to_string(),
                node_id: input.node_id.trim().to_string(),
                interface_name: input.interface_name.trim().to_string(),
                billing_direction,
                traffic_limit_bytes,
                initial_used_bytes,
                reset_policy,
                reset_anchor,
                current_cycle_start: cycle.start,
                current_cycle_end: cycle.end,
            },
            now,
        )
        .await?;
    Ok(format!(
        "added NIC binding to subscription {subscription_id}"
    ))
}

async fn update_subscription(
    database: &Database,
    input: EditSubscriptionInput,
) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let name = input.name.trim().to_string();
    if name.is_empty() {
        anyhow::bail!("subscription name is required");
    }
    let node_ids = input
        .node_ids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let traffic_limit_bytes = if input.limit_bytes.trim().is_empty() {
        None
    } else {
        Some(
            input
                .limit_bytes
                .trim()
                .parse::<i64>()
                .context("invalid limit bytes")?,
        )
    };
    let expires_at = if input.expires_at.trim().is_empty() {
        None
    } else {
        Some(
            input
                .expires_at
                .trim()
                .parse::<i64>()
                .context("invalid expiry unix time")?,
        )
    };
    let multiplier = match input.multiplier.trim() {
        "1" | "1x" => 10_000,
        "2" | "2x" => 20_000,
        _ => anyhow::bail!("multiplier must be 1 or 2"),
    };
    let status = input.status.trim().to_ascii_lowercase();
    if !matches!(status.as_str(), "active" | "disabled") {
        anyhow::bail!("status must be active or disabled");
    }
    let policy = xenon_domain::ResetPolicy::parse(&input.reset_policy, now)
        .context("invalid reset policy")?;
    let (reset_policy, reset_anchor) = policy.stored();
    let next_cycle_end = policy
        .cycle_at(input.starts_at, now)
        .context("calculate next billing boundary")?
        .end;
    if next_cycle_end.is_some_and(|end| end <= input.current_cycle_start) {
        anyhow::bail!("new reset policy ends before the current cycle");
    }
    if !database
        .update_subscription(
            &input.subscription_id,
            &models::UpdateSubscription {
                name,
                status,
                expires_at,
                traffic_limit_bytes,
                traffic_multiplier_basis_points: multiplier,
                reset_policy,
                reset_anchor,
                current_cycle_end: next_cycle_end,
                node_ids,
                updated_at: now,
            },
        )
        .await?
    {
        anyhow::bail!("subscription no longer exists");
    }
    Ok(format!("updated subscription {}", input.subscription_id))
}

async fn rotate_subscription_token(
    database: &Database,
    subscription_id: &str,
) -> anyhow::Result<String> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    if !database
        .rotate_subscription_token(
            subscription_id,
            &sha256_hex(token.as_bytes()),
            Utc::now().timestamp(),
        )
        .await?
    {
        anyhow::bail!("active subscription no longer exists");
    }
    Ok(format!(
        "subscription token rotated; save this token now: {token}"
    ))
}

async fn rotate_subscription_uuid(
    database: &Database,
    subscription_id: &str,
) -> anyhow::Result<String> {
    let uuid = Uuid::new_v4().to_string();
    if !database
        .rotate_subscription_uuid(subscription_id, &uuid, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("subscription no longer exists");
    }
    Ok(format!("subscription UUID rotated; new UUID: {uuid}"))
}

async fn update_host(
    database: &Database,
    node_id: &str,
    input: CreateHostInput,
) -> anyhow::Result<String> {
    if !database
        .update_managed_host(
            node_id,
            &models::UpdateManagedHost {
                name: input.name.trim().to_string(),
                landing_host: input.landing_host.trim().to_string(),
                updated_at: Utc::now().timestamp(),
            },
        )
        .await?
    {
        anyhow::bail!("host no longer exists");
    }
    Ok(format!("updated host {node_id}"))
}

async fn set_host_status(
    database: &Database,
    node_id: &str,
    status: &str,
) -> anyhow::Result<String> {
    if !database
        .set_node_management_status(node_id, status, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("host no longer exists");
    }
    Ok(format!("host {node_id} is now {status}"))
}

async fn delete_host(database: &Database, node_id: &str) -> anyhow::Result<String> {
    if !database
        .delete_node(node_id, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("host no longer exists");
    }
    Ok(format!("logically deleted host {node_id}"))
}

fn proxy_node_model(input: ProxyNodeInput) -> anyhow::Result<models::UpdateProxyNode> {
    let listen_port = input
        .listen_port
        .trim()
        .parse::<i64>()
        .context("invalid Xray listen port")?;
    let publish_host =
        (!input.publish_host.trim().is_empty()).then(|| input.publish_host.trim().to_string());
    let publish_port = if input.publish_port.trim().is_empty() {
        None
    } else {
        Some(
            input
                .publish_port
                .trim()
                .parse::<i64>()
                .context("invalid publish port")?,
        )
    };
    if publish_host.is_some() != publish_port.is_some() {
        anyhow::bail!("publish host and port must be entered together");
    }
    let optional = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_string());
    let (protocol, transport, security) = match input.profile.trim() {
        "vless-reality" => ("vless", "tcp", "reality"),
        "vless-encryption" => ("vless", "tcp", "none"),
        "vless-ws" => ("vless", "ws", "none"),
        "ss-2022" => {
            anyhow::bail!("SS2022 credentials and Agent deployment are not implemented yet")
        }
        _ => anyhow::bail!("unknown Xray node profile"),
    };
    Ok(models::UpdateProxyNode {
        host_id: input.host_id.trim().to_string(),
        name: input.name.trim().to_string(),
        listen_port,
        publish_host,
        publish_port,
        protocol: protocol.into(),
        transport: transport.into(),
        security: security.into(),
        server_name: optional(input.server_name),
        websocket_path: optional(input.websocket_path),
        vless_encryption: optional(input.vless_encryption),
        reality_public_key: optional(input.reality_public_key),
        reality_short_id: optional(input.reality_short_id),
        reality_fingerprint: optional(input.reality_fingerprint),
        updated_at: Utc::now().timestamp(),
    })
}

async fn create_proxy_node(database: &Database, input: ProxyNodeInput) -> anyhow::Result<String> {
    let node = proxy_node_model(input)?;
    let proxy_node_id = Uuid::now_v7().to_string();
    database
        .create_proxy_node_with_status(
            &models::NewProxyNode {
                id: proxy_node_id.clone(),
                host_id: node.host_id,
                name: node.name,
                listen_port: node.listen_port,
                publish_host: node.publish_host,
                publish_port: node.publish_port,
                protocol: node.protocol,
                transport: node.transport,
                security: node.security,
                server_name: node.server_name,
                websocket_path: node.websocket_path,
                vless_encryption: node.vless_encryption,
                reality_public_key: node.reality_public_key,
                reality_short_id: node.reality_short_id,
                reality_fingerprint: node.reality_fingerprint,
                created_at: node.updated_at,
            },
            "disabled",
        )
        .await?;
    Ok(format!(
        "saved disabled Xray node {proxy_node_id}; Agent multi-inbound deployment is pending"
    ))
}

async fn update_proxy_node(
    database: &Database,
    proxy_node_id: &str,
    input: ProxyNodeInput,
) -> anyhow::Result<String> {
    if !database
        .update_proxy_node(proxy_node_id, &proxy_node_model(input)?)
        .await?
    {
        anyhow::bail!("Xray node no longer exists");
    }
    Ok(format!("updated Xray node {proxy_node_id}"))
}

async fn set_proxy_node_status(
    database: &Database,
    proxy_node_id: &str,
    status: &str,
) -> anyhow::Result<String> {
    if !database
        .set_proxy_node_status(proxy_node_id, status, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("Xray node no longer exists");
    }
    Ok(format!("Xray node {proxy_node_id} is now {status}"))
}

async fn delete_proxy_node(database: &Database, proxy_node_id: &str) -> anyhow::Result<String> {
    if !database
        .delete_proxy_node(proxy_node_id, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("Xray node no longer exists");
    }
    Ok(format!("logically deleted Xray node {proxy_node_id}"))
}

async fn load_snapshot(
    state: &Arc<RwLock<RuntimeState>>,
    database: &Database,
    notice: String,
    user_detail: Option<models::UserDetail>,
) -> anyhow::Result<TuiSnapshot> {
    database
        .advance_billing_cycles(Utc::now().timestamp())
        .await?;
    let (connected_agents, last_agent_event, agent_events) = {
        let runtime = state.read().await;
        (
            runtime.connected_agents,
            runtime.last_agent_event.clone(),
            runtime.agent_events.iter().cloned().collect(),
        )
    };
    Ok(TuiSnapshot {
        connected_agents,
        last_agent_event,
        users: database.list_user_summaries(Utc::now().timestamp()).await?,
        nodes: database.list_node_overviews().await?,
        proxy_nodes: database.list_proxy_nodes().await?,
        interfaces: database.list_recent_interfaces().await?,
        agent_events,
        user_detail,
        notice,
        ..TuiSnapshot::default()
    })
}

async fn create_subscription(
    database: &Database,
    subscription_base_url: &str,
    input: CreateSubscriptionInput,
) -> anyhow::Result<String> {
    let username = if input.username.trim().is_empty() {
        "admin".to_string()
    } else {
        input.username.trim().to_string()
    };
    let name = input.name.trim().to_string();
    if name.is_empty() {
        anyhow::bail!("subscription name is required");
    }
    let node_ids = input
        .node_ids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if node_ids.is_empty() {
        anyhow::bail!("at least one node ID is required");
    }
    let traffic_limit_bytes = if input.limit_bytes.trim().is_empty() {
        None
    } else {
        Some(
            input
                .limit_bytes
                .trim()
                .parse::<i64>()
                .context("invalid limit bytes")?,
        )
    };
    if traffic_limit_bytes.is_some_and(|value| value <= 0) {
        anyhow::bail!("limit bytes must be greater than zero");
    }
    let expires_at = if input.expires_at.trim().is_empty() {
        None
    } else {
        Some(
            input
                .expires_at
                .trim()
                .parse::<i64>()
                .context("invalid expiry unix time")?,
        )
    };
    let multiplier = match input.multiplier.trim() {
        "" | "1" | "1x" => 10_000,
        "2" | "2x" => 20_000,
        _ => anyhow::bail!("multiplier must be 1 or 2"),
    };
    let now = Utc::now().timestamp();
    if expires_at.is_some_and(|value| value <= now) {
        anyhow::bail!("expiry must be in the future");
    }
    let reset_policy = xenon_domain::ResetPolicy::parse(&input.reset_policy, now)
        .context("invalid reset policy")?;
    let (reset_policy_name, reset_anchor) = reset_policy.stored();
    let cycle = reset_policy
        .cycle_at(now, now)
        .context("calculate initial billing cycle")?;
    let mut nic_bindings = Vec::new();
    for value in input
        .nic_bindings
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let parts = value.split('/').collect::<Vec<_>>();
        if !(4..=6).contains(&parts.len()) {
            anyhow::bail!("NIC binding format: node/interface/limit/initial[/direction[/reset]]");
        }
        let limit = parts[2].parse::<i64>().context("invalid NIC limit")?;
        let initial = parts[3]
            .parse::<i64>()
            .context("invalid NIC initial usage")?;
        let direction = parts.get(4).map_or("rx_tx", |value| value.trim());
        if !matches!(direction, "rx_tx" | "tx_only" | "rx_only") {
            anyhow::bail!("NIC direction must be rx_tx, tx_only, or rx_only");
        }
        let binding_policy = match parts.get(5).map(|value| value.trim()) {
            Some("") | None => reset_policy,
            Some(value) => {
                xenon_domain::ResetPolicy::parse(value, now).context("invalid NIC reset policy")?
            }
        };
        let (binding_policy_name, binding_anchor) = binding_policy.stored();
        let binding_cycle = binding_policy
            .cycle_at(now, now)
            .context("calculate initial NIC billing cycle")?;
        nic_bindings.push(models::NewNicBinding {
            id: Uuid::now_v7().to_string(),
            node_id: parts[0].trim().to_string(),
            interface_name: parts[1].trim().to_string(),
            billing_direction: direction.into(),
            traffic_limit_bytes: limit,
            initial_used_bytes: initial,
            reset_policy: binding_policy_name,
            reset_anchor: binding_anchor,
            current_cycle_start: binding_cycle.start,
            current_cycle_end: binding_cycle.end,
        });
    }

    let subscription_id = Uuid::now_v7().to_string();
    let user_id = Uuid::now_v7().to_string();
    let xray_uuid = Uuid::new_v4().to_string();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let xray_email = format!("sub-{subscription_id}@panel");
    database
        .create_user_subscription(&models::NewSubscription {
            user_id,
            username,
            subscription_id: subscription_id.clone(),
            name,
            token_hash: sha256_hex(token.as_bytes()),
            xray_uuid,
            xray_email,
            starts_at: now,
            expires_at,
            traffic_limit_bytes,
            traffic_multiplier_basis_points: multiplier,
            reset_policy: reset_policy_name,
            reset_anchor,
            current_cycle_start: cycle.start,
            current_cycle_end: cycle.end,
            node_ids,
            nic_bindings,
            created_at: now,
        })
        .await
        .context("store user and subscription")?;

    Ok(format!(
        "created subscription {subscription_id}; URL: {subscription_base_url}/sub/{token}"
    ))
}

async fn create_host(
    database: &Database,
    grpc_addr: &str,
    agent_install: &AgentInstallConfig,
    input: CreateHostInput,
) -> anyhow::Result<String> {
    let name = input.name.trim().to_string();
    let landing_host = input.landing_host.trim().to_string();
    let now = Utc::now().timestamp();
    let node_id = Uuid::now_v7().to_string();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    database
        .create_managed_host_with_registration(
            &models::NewManagedHost {
                id: node_id.clone(),
                name,
                landing_host,
                created_at: now,
            },
            &models::NewRegistrationToken {
                id: Uuid::now_v7().to_string(),
                node_id: node_id.clone(),
                token_hash: sha256_hex(token.as_bytes()),
                expires_at: now + 3600,
                created_at: now,
            },
        )
        .await
        .context("store host and registration token")?;
    agent_install_notice(
        grpc_addr,
        agent_install,
        &node_id,
        &token,
        &format!("已创建主机 {node_id}"),
    )
}

async fn create_host_registration(
    database: &Database,
    grpc_addr: &str,
    agent_install: &AgentInstallConfig,
    node_id: &str,
) -> anyhow::Result<String> {
    let now = Utc::now().timestamp();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    database
        .create_registration_token(&models::NewRegistrationToken {
            id: Uuid::now_v7().to_string(),
            node_id: node_id.to_string(),
            token_hash: sha256_hex(token.as_bytes()),
            expires_at: now + 3600,
            created_at: now,
        })
        .await
        .context("store Agent registration token")?;
    agent_install_notice(
        grpc_addr,
        agent_install,
        node_id,
        &token,
        &format!("已为主机 {node_id} 签发新的 Agent 注册凭据"),
    )
}

fn agent_install_notice(
    grpc_addr: &str,
    agent_install: &AgentInstallConfig,
    node_id: &str,
    token: &str,
    summary: &str,
) -> anyhow::Result<String> {
    if agent_install.enabled {
        let binary_args = agent_binary_args(agent_install)?;
        let ca_arg = agent_ca_arg(agent_install)?;
        Ok(format!(
            "{summary}; install: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo bash -s -- --panel '{}' --enrollment '{}' --server-name '{}' --node '{}' --token '{}' {binary_args} {ca_arg}",
            agent_install.script_url,
            agent_install.panel_endpoint,
            agent_install.enrollment_endpoint,
            agent_install.server_name,
            node_id,
            token,
        ))
    } else {
        Ok(format!(
            "{summary}; installer-disabled; token: {token}; panel: {grpc_addr}"
        ))
    }
}

fn agent_binary_args(agent_install: &AgentInstallConfig) -> anyhow::Result<String> {
    if agent_install.binary_url.is_empty() {
        anyhow::bail!("agent_install.binary_url is not configured");
    }
    let mut args = format!("--binary-url '{}'", agent_install.binary_url);
    let mut pinned = false;
    if !agent_install.binary_sha256_x86_64.is_empty() {
        args.push_str(&format!(
            " --binary-sha256-x86-64 '{}'",
            agent_install.binary_sha256_x86_64
        ));
        pinned = true;
    }
    if !agent_install.binary_sha256_aarch64.is_empty() {
        args.push_str(&format!(
            " --binary-sha256-aarch64 '{}'",
            agent_install.binary_sha256_aarch64
        ));
        pinned = true;
    }
    if !agent_install.binary_sha256.is_empty() {
        args.push_str(&format!(
            " --binary-sha256 '{}'",
            agent_install.binary_sha256
        ));
        pinned = true;
    }
    if !pinned {
        anyhow::bail!("agent_install has no pinned binary SHA-256");
    }
    if !agent_install.binary_version.is_empty() {
        args.push_str(&format!(
            " --agent-version '{}'",
            agent_install.binary_version
        ));
    }
    Ok(args)
}

fn agent_ca_arg(agent_install: &AgentInstallConfig) -> anyhow::Result<String> {
    if !agent_install.ca_path.is_empty() {
        let pem = std::fs::read(&agent_install.ca_path)
            .with_context(|| format!("read agent CA at {}", agent_install.ca_path))?;
        if !pem.starts_with(b"-----BEGIN CERTIFICATE") {
            anyhow::bail!("agent_install.ca_path is not a PEM certificate");
        }
        use base64::Engine as _;
        return Ok(format!(
            "--ca-b64 '{}'",
            base64::engine::general_purpose::STANDARD.encode(pem)
        ));
    }
    if !agent_install.ca_url.is_empty() {
        return Ok(format!("--ca-url '{}'", agent_install.ca_url));
    }
    anyhow::bail!("agent_install needs ca_path or ca_url")
}

fn agent_upgrade_command(
    agent_install: &AgentInstallConfig,
    node_id: &str,
) -> anyhow::Result<String> {
    if !agent_install.enabled {
        anyhow::bail!("Agent release source is not configured");
    }
    let binary_args = agent_binary_args(agent_install)?;
    Ok(format!(
        "node {node_id}; upgrade: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo bash -s -- --upgrade {binary_args}; rollback: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo bash -s -- --rollback",
        agent_install.script_url, agent_install.script_url,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Users,
    Nodes,
    Hosts,
    Logs,
    Create,
    HostCreate,
    HostCreateResult,
    ProxyNodeCreate,
    Revoke,
    UserDetail,
    NicBindings,
    NicCreate,
    NicUnbindConfirm,
    SubscriptionEdit,
    SubscriptionNodes,
    SubscriptionRotateConfirm,
    HostEdit,
    HostDeleteConfirm,
    ProxyNodeEdit,
    ProxyNodeDeleteConfirm,
}

#[derive(Debug, Clone, Copy)]
enum RotateKind {
    Token,
    Uuid,
}

struct FormState {
    fields: [String; 8],
    active: usize,
}

#[derive(Default)]
struct HostFormState {
    fields: [String; 2],
    active: usize,
}

struct ProxyNodeFormState {
    fields: [String; 11],
    profile: usize,
    active: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyNodeFormItem {
    Tag,
    Protocol,
    Host,
    Port,
    ServerName,
    WebSocketPath,
}

struct NicBindingFormState {
    fields: [String; 6],
    active: usize,
}

struct SubscriptionEditFormState {
    fields: [String; 7],
    subscription_id: String,
    starts_at: i64,
    current_cycle_start: i64,
    active: usize,
}

#[derive(Default)]
struct NodeAssignmentState {
    cursor: usize,
    selected_ids: Vec<String>,
}

impl NodeAssignmentState {
    fn from_snapshot(snapshot: &TuiSnapshot, selected_subscription: usize) -> Option<Self> {
        let detail = snapshot.user_detail.as_ref()?;
        let subscription = detail.subscriptions.get(selected_subscription)?;
        Some(Self {
            cursor: 0,
            selected_ids: detail
                .proxy_nodes
                .iter()
                .filter(|assignment| assignment.subscription_id == subscription.id)
                .map(|assignment| assignment.proxy_node_id.clone())
                .collect(),
        })
    }

    fn contains(&self, node_id: &str) -> bool {
        self.selected_ids.iter().any(|selected| selected == node_id)
    }

    fn toggle(&mut self, node: &models::ProxyNodeRecord) {
        if let Some(index) = self
            .selected_ids
            .iter()
            .position(|selected| selected == &node.id)
        {
            self.selected_ids.remove(index);
        } else if node.status == "active" {
            self.selected_ids.push(node.id.clone());
        }
    }
}

impl SubscriptionEditFormState {
    fn from_snapshot(snapshot: &TuiSnapshot, selected_subscription: usize) -> Option<Self> {
        let detail = snapshot.user_detail.as_ref()?;
        let subscription = detail.subscriptions.get(selected_subscription)?;
        let node_ids = detail
            .proxy_nodes
            .iter()
            .filter(|assignment| assignment.subscription_id == subscription.id)
            .map(|assignment| assignment.proxy_node_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        Some(Self {
            fields: [
                subscription.name.clone(),
                node_ids,
                subscription
                    .traffic_limit_bytes
                    .map_or_else(String::new, |value| value.to_string()),
                subscription
                    .expires_at
                    .map_or_else(String::new, |value| value.to_string()),
                format!(
                    "{}",
                    subscription.traffic_multiplier_basis_points as f64 / 10_000.0
                ),
                format_reset_policy(&subscription.reset_policy, subscription.reset_anchor),
                subscription.status.clone(),
            ],
            subscription_id: subscription.id.clone(),
            starts_at: subscription.starts_at,
            current_cycle_start: subscription.current_cycle_start,
            active: 0,
        })
    }

    fn input(&self) -> EditSubscriptionInput {
        EditSubscriptionInput {
            subscription_id: self.subscription_id.clone(),
            starts_at: self.starts_at,
            current_cycle_start: self.current_cycle_start,
            name: self.fields[0].clone(),
            node_ids: self.fields[1].clone(),
            limit_bytes: self.fields[2].clone(),
            expires_at: self.fields[3].clone(),
            multiplier: self.fields[4].clone(),
            reset_policy: self.fields[5].clone(),
            status: self.fields[6].clone(),
        }
    }
}

impl NicBindingFormState {
    fn from_snapshot(snapshot: &TuiSnapshot, subscription_id: Option<&str>) -> Self {
        let node_id = snapshot
            .user_detail
            .as_ref()
            .and_then(|detail| {
                subscription_id.and_then(|id| {
                    detail
                        .node_usage
                        .iter()
                        .find(|usage| usage.subscription_id == id)
                        .map(|usage| usage.node_id.clone())
                })
            })
            .or_else(|| snapshot.nodes.first().map(|node| node.id.clone()))
            .unwrap_or_default();
        let interface_name = snapshot
            .interfaces
            .iter()
            .find(|interface| interface.node_id == node_id)
            .map(|interface| interface.interface_name.clone())
            .unwrap_or_default();
        Self {
            fields: [
                node_id,
                interface_name,
                String::new(),
                "0".into(),
                "rx_tx".into(),
                "never".into(),
            ],
            active: 0,
        }
    }

    fn input(&self) -> CreateNicBindingInput {
        CreateNicBindingInput {
            node_id: self.fields[0].clone(),
            interface_name: self.fields[1].clone(),
            traffic_limit_bytes: self.fields[2].clone(),
            initial_used_bytes: self.fields[3].clone(),
            billing_direction: self.fields[4].clone(),
            reset_policy: self.fields[5].clone(),
        }
    }
}

impl HostFormState {
    fn from_node(node: &models::NodeOverview) -> Self {
        Self {
            fields: [node.name.clone(), node.landing_host.clone()],
            active: 0,
        }
    }

    fn input(&self) -> CreateHostInput {
        CreateHostInput {
            name: self.fields[0].clone(),
            landing_host: self.fields[1].clone(),
        }
    }
}

const PROXY_NODE_PROFILES: [&str; 4] = ["vless-reality", "vless-encryption", "vless-ws", "ss-2022"];

impl ProxyNodeFormState {
    const REALITY_ITEMS: [ProxyNodeFormItem; 5] = [
        ProxyNodeFormItem::Tag,
        ProxyNodeFormItem::Protocol,
        ProxyNodeFormItem::Host,
        ProxyNodeFormItem::Port,
        ProxyNodeFormItem::ServerName,
    ];
    const ENCRYPTION_ITEMS: [ProxyNodeFormItem; 4] = [
        ProxyNodeFormItem::Tag,
        ProxyNodeFormItem::Protocol,
        ProxyNodeFormItem::Host,
        ProxyNodeFormItem::Port,
    ];
    const WS_ITEMS: [ProxyNodeFormItem; 5] = [
        ProxyNodeFormItem::Tag,
        ProxyNodeFormItem::Protocol,
        ProxyNodeFormItem::Host,
        ProxyNodeFormItem::Port,
        ProxyNodeFormItem::WebSocketPath,
    ];
    const SS_ITEMS: [ProxyNodeFormItem; 4] = [
        ProxyNodeFormItem::Tag,
        ProxyNodeFormItem::Protocol,
        ProxyNodeFormItem::Host,
        ProxyNodeFormItem::Port,
    ];

    fn new(snapshot: &TuiSnapshot) -> Self {
        Self {
            fields: [
                snapshot
                    .nodes
                    .first()
                    .map(|node| node.id.clone())
                    .unwrap_or_default(),
                String::new(),
                "443".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                "chrome".into(),
            ],
            profile: 0,
            active: 0,
        }
    }

    fn from_node(node: &models::ProxyNodeRecord) -> Self {
        let profile = match (
            node.protocol.as_str(),
            node.transport.as_str(),
            node.security.as_str(),
            node.vless_encryption.as_deref(),
        ) {
            ("vless", "tcp", "reality", _) => 0,
            ("vless", "tcp", _, Some(_)) => 1,
            ("vless", "ws", _, _) => 2,
            ("shadowsocks", _, _, _) => 3,
            _ => 1,
        };
        Self {
            fields: [
                node.host_id.clone(),
                node.name.clone(),
                node.listen_port.to_string(),
                node.publish_host.clone().unwrap_or_default(),
                node.publish_port
                    .map_or_else(String::new, |port| port.to_string()),
                node.server_name.clone().unwrap_or_default(),
                node.websocket_path.clone().unwrap_or_default(),
                node.vless_encryption.clone().unwrap_or_default(),
                node.reality_public_key.clone().unwrap_or_default(),
                node.reality_short_id.clone().unwrap_or_default(),
                node.reality_fingerprint
                    .clone()
                    .unwrap_or_else(|| "chrome".into()),
            ],
            profile,
            active: 0,
        }
    }

    fn input(&self) -> ProxyNodeInput {
        let generated_short_id = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>();
        ProxyNodeInput {
            host_id: self.fields[0].clone(),
            name: self.fields[1].clone(),
            profile: PROXY_NODE_PROFILES[self.profile].into(),
            listen_port: self.fields[2].clone(),
            publish_host: self.fields[3].clone(),
            publish_port: self.fields[4].clone(),
            server_name: self.fields[5].clone(),
            websocket_path: self.fields[6].clone(),
            vless_encryption: self.fields[7].clone(),
            reality_public_key: self.fields[8].clone(),
            reality_short_id: if self.profile == 0 && self.fields[9].trim().is_empty() {
                generated_short_id
            } else {
                self.fields[9].clone()
            },
            reality_fingerprint: if self.profile == 0 {
                "chrome".into()
            } else {
                self.fields[10].clone()
            },
        }
    }

    fn cycle_profile(&mut self, forward: bool) {
        self.profile = if forward {
            (self.profile + 1) % PROXY_NODE_PROFILES.len()
        } else {
            (self.profile + PROXY_NODE_PROFILES.len() - 1) % PROXY_NODE_PROFILES.len()
        };
        self.active = self
            .active
            .min(self.visible_items().len().saturating_sub(1));
    }

    fn visible_items(&self) -> &'static [ProxyNodeFormItem] {
        match self.profile {
            0 => &Self::REALITY_ITEMS,
            1 => &Self::ENCRYPTION_ITEMS,
            2 => &Self::WS_ITEMS,
            _ => &Self::SS_ITEMS,
        }
    }

    fn active_item(&self) -> ProxyNodeFormItem {
        self.visible_items()[self.active.min(self.visible_items().len() - 1)]
    }

    fn editable_field(&self) -> Option<usize> {
        match self.active_item() {
            ProxyNodeFormItem::Tag => Some(1),
            ProxyNodeFormItem::Port => Some(2),
            ProxyNodeFormItem::ServerName => Some(5),
            ProxyNodeFormItem::WebSocketPath => Some(6),
            ProxyNodeFormItem::Protocol | ProxyNodeFormItem::Host => None,
        }
    }

    fn cycle_host(&mut self, snapshot: &TuiSnapshot, forward: bool) {
        if snapshot.nodes.is_empty() {
            self.fields[0].clear();
            return;
        }
        let current = snapshot
            .nodes
            .iter()
            .position(|host| host.id == self.fields[0])
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % snapshot.nodes.len()
        } else {
            (current + snapshot.nodes.len() - 1) % snapshot.nodes.len()
        };
        self.fields[0].clone_from(&snapshot.nodes[next].id);
    }

    fn handle_choice(&mut self, snapshot: &TuiSnapshot, forward: bool) {
        match self.active_item() {
            ProxyNodeFormItem::Protocol => self.cycle_profile(forward),
            ProxyNodeFormItem::Host => self.cycle_host(snapshot, forward),
            _ => {}
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let count = self.visible_items().len();
        self.active = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
    }
}

impl FormState {
    fn from_snapshot(snapshot: &TuiSnapshot) -> Self {
        let node_ids = snapshot
            .proxy_nodes
            .iter()
            .filter(|node| node.status == "active")
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        Self {
            fields: [
                "admin".into(),
                String::new(),
                node_ids,
                String::new(),
                String::new(),
                "1".into(),
                "never".into(),
                String::new(),
            ],
            active: 0,
        }
    }

    fn input(&self) -> CreateSubscriptionInput {
        CreateSubscriptionInput {
            username: self.fields[0].clone(),
            name: self.fields[1].clone(),
            node_ids: self.fields[2].clone(),
            limit_bytes: self.fields[3].clone(),
            expires_at: self.fields[4].clone(),
            multiplier: self.fields[5].clone(),
            reset_policy: self.fields[6].clone(),
            nic_bindings: self.fields[7].clone(),
        }
    }
}

fn run_blocking(
    snapshot_rx: watch::Receiver<TuiSnapshot>,
    command_tx: mpsc::Sender<TuiCommand>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut page = Page::Dashboard;
    let mut form = FormState {
        fields: Default::default(),
        active: 0,
    };
    let mut host_form = HostFormState::default();
    let mut proxy_node_form = ProxyNodeFormState::new(&snapshot_rx.borrow());
    let mut nic_form = NicBindingFormState::from_snapshot(&snapshot_rx.borrow(), None);
    let mut subscription_form = None;
    let mut node_assignment = NodeAssignmentState::default();
    let mut rotate_kind = RotateKind::Token;
    let mut revoke_node_id = String::new();
    let mut revoke_returns_to_nodes = false;
    let mut selected_user = 0_usize;
    let mut selected_subscription = 0_usize;
    let mut selected_binding = 0_usize;
    let mut selected_node = 0_usize;
    let mut selected_proxy_node = 0_usize;
    let mut edit_host_id = String::new();
    let mut delete_host_id = String::new();
    let mut edit_proxy_node_id = String::new();
    let mut delete_proxy_node_id = String::new();
    let mut unbind_binding_id = String::new();
    let mut pending_host_notice = None::<String>;
    let mut host_create_result = String::new();
    let mut dismissed_notice = None::<String>;
    let mut last_seen_notice = String::new();
    let result = loop {
        let mut snapshot = snapshot_rx.borrow().clone();
        let raw_notice = snapshot.notice.clone();
        last_seen_notice.clone_from(&raw_notice);
        if pending_host_notice
            .as_deref()
            .is_some_and(|previous| !raw_notice.is_empty() && raw_notice != previous)
        {
            host_create_result.clone_from(&raw_notice);
            pending_host_notice = None;
            page = Page::HostCreateResult;
        }
        if dismissed_notice.as_deref() == Some(raw_notice.as_str()) {
            snapshot.notice.clear();
        }
        selected_user = selected_user.min(snapshot.users.len().saturating_sub(1));
        if let Some(detail) = snapshot.user_detail.as_ref() {
            selected_subscription =
                selected_subscription.min(detail.subscriptions.len().saturating_sub(1));
        }
        selected_node = selected_node.min(snapshot.nodes.len().saturating_sub(1));
        selected_proxy_node = selected_proxy_node.min(snapshot.proxy_nodes.len().saturating_sub(1));
        node_assignment.cursor = node_assignment
            .cursor
            .min(snapshot.proxy_nodes.len().saturating_sub(1));
        terminal.draw(|frame| match page {
            Page::Dashboard => {
                let area = draw_primary_shell(frame, &snapshot, 0);
                draw_dashboard(frame, area, &snapshot);
            }
            Page::Users => {
                let area = draw_primary_shell(frame, &snapshot, 1);
                draw_users(frame, area, &snapshot, selected_user);
            }
            Page::Create => {
                let area = draw_primary_shell(frame, &snapshot, 1);
                draw_users(frame, area, &snapshot, selected_user);
                draw_create(frame, &snapshot, &form);
            }
            Page::HostCreate => {
                let area = draw_primary_shell(frame, &snapshot, 3);
                draw_nodes(frame, area, &snapshot, selected_node);
                draw_host_create(frame, &host_form);
            }
            Page::HostCreateResult => {
                let area = draw_primary_shell(frame, &snapshot, 3);
                draw_nodes(frame, area, &snapshot, selected_node);
                draw_host_create_result(frame, &host_create_result);
            }
            Page::ProxyNodeCreate => {
                let area = draw_primary_shell(frame, &snapshot, 2);
                draw_proxy_nodes(frame, area, &snapshot, selected_proxy_node);
                draw_proxy_node_create(frame, &snapshot, &proxy_node_form);
            }
            Page::Revoke => {
                let area = draw_primary_shell(
                    frame,
                    &snapshot,
                    if revoke_returns_to_nodes { 3 } else { 0 },
                );
                if revoke_returns_to_nodes {
                    draw_nodes(frame, area, &snapshot, selected_node);
                } else {
                    draw_dashboard(frame, area, &snapshot);
                }
                draw_revoke(frame, &snapshot, &revoke_node_id);
            }
            Page::UserDetail => draw_user_detail(frame, &snapshot, selected_subscription),
            Page::NicBindings => {
                draw_nic_bindings(frame, &snapshot, selected_subscription, selected_binding)
            }
            Page::NicCreate => {
                draw_nic_bindings(frame, &snapshot, selected_subscription, selected_binding);
                draw_nic_create(frame, &snapshot, &nic_form);
            }
            Page::NicUnbindConfirm => {
                draw_nic_bindings(frame, &snapshot, selected_subscription, selected_binding);
                draw_nic_unbind_confirm(frame, &snapshot, &unbind_binding_id);
            }
            Page::SubscriptionEdit => {
                draw_user_detail(frame, &snapshot, selected_subscription);
                if let Some(form) = subscription_form.as_ref() {
                    draw_subscription_edit(frame, form);
                }
            }
            Page::SubscriptionNodes => {
                draw_user_detail(frame, &snapshot, selected_subscription);
                draw_node_assignment(frame, &snapshot, selected_subscription, &node_assignment);
            }
            Page::SubscriptionRotateConfirm => {
                draw_user_detail(frame, &snapshot, selected_subscription);
                draw_subscription_rotate_confirm(
                    frame,
                    &snapshot,
                    selected_subscription,
                    rotate_kind,
                );
            }
            Page::Nodes => {
                let area = draw_primary_shell(frame, &snapshot, 2);
                draw_proxy_nodes(frame, area, &snapshot, selected_proxy_node);
            }
            Page::Hosts => {
                let area = draw_primary_shell(frame, &snapshot, 3);
                draw_nodes(frame, area, &snapshot, selected_node);
            }
            Page::Logs => {
                let area = draw_primary_shell(frame, &snapshot, 4);
                draw_logs(frame, area, &snapshot);
            }
            Page::HostEdit => {
                let area = draw_primary_shell(frame, &snapshot, 3);
                draw_nodes(frame, area, &snapshot, selected_node);
                draw_host_edit(frame, &host_form, &edit_host_id);
            }
            Page::HostDeleteConfirm => {
                let area = draw_primary_shell(frame, &snapshot, 3);
                draw_nodes(frame, area, &snapshot, selected_node);
                draw_host_delete_confirm(frame, &snapshot, &delete_host_id);
            }
            Page::ProxyNodeEdit => {
                let area = draw_primary_shell(frame, &snapshot, 2);
                draw_proxy_nodes(frame, area, &snapshot, selected_proxy_node);
                draw_proxy_node_edit(frame, &snapshot, &proxy_node_form, &edit_proxy_node_id);
            }
            Page::ProxyNodeDeleteConfirm => {
                let area = draw_primary_shell(frame, &snapshot, 2);
                draw_proxy_nodes(frame, area, &snapshot, selected_proxy_node);
                draw_proxy_node_delete_confirm(frame, &delete_proxy_node_id);
            }
        })?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match page {
                    Page::Dashboard => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Tab | KeyCode::Char('2') => {
                            page = Page::Users;
                        }
                        KeyCode::Char('3') | KeyCode::Char('N') => {
                            selected_proxy_node = 0;
                            page = Page::Nodes;
                        }
                        KeyCode::Char('4') => {
                            selected_node = 0;
                            page = Page::Hosts;
                        }
                        KeyCode::Char('5') => page = Page::Logs,
                        KeyCode::Char('a') | KeyCode::Char('c') => {
                            form = FormState::from_snapshot(&snapshot);
                            page = Page::Create;
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        KeyCode::Char('r') => {
                            revoke_returns_to_nodes = false;
                            revoke_node_id = snapshot
                                .nodes
                                .first()
                                .map(|node| node.id.clone())
                                .unwrap_or_default();
                            page = Page::Revoke;
                        }
                        _ => {}
                    },
                    Page::Users => match key.code {
                        KeyCode::Char('q') => break Ok(()),
                        KeyCode::Esc | KeyCode::Char('1') => page = Page::Dashboard,
                        KeyCode::Tab | KeyCode::Char('3') => {
                            selected_proxy_node = 0;
                            page = Page::Nodes;
                        }
                        KeyCode::Char('4') => {
                            selected_node = 0;
                            page = Page::Hosts;
                        }
                        KeyCode::Char('5') => page = Page::Logs,
                        KeyCode::Char('a') | KeyCode::Char('c') => {
                            form = FormState::from_snapshot(&snapshot);
                            page = Page::Create;
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        KeyCode::Up => {
                            selected_user = selected_user.saturating_sub(1);
                        }
                        KeyCode::Down if !snapshot.users.is_empty() => {
                            selected_user = (selected_user + 1).min(snapshot.users.len() - 1);
                        }
                        KeyCode::Enter => {
                            if let Some(user) = snapshot.users.get(selected_user) {
                                if command_tx
                                    .blocking_send(TuiCommand::ShowUser(user.id.clone()))
                                    .is_err()
                                {
                                    break Ok(());
                                }
                                selected_subscription = 0;
                                selected_binding = 0;
                                page = Page::UserDetail;
                            }
                        }
                        _ => {}
                    },
                    Page::Create => match key.code {
                        KeyCode::Esc => page = Page::Users,
                        KeyCode::Tab | KeyCode::Down => form.active = (form.active + 1) % 8,
                        KeyCode::BackTab | KeyCode::Up => form.active = (form.active + 7) % 8,
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::CreateSubscription(form.input()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Users;
                        }
                        KeyCode::Backspace => {
                            form.fields[form.active].pop();
                        }
                        KeyCode::Char(value) => form.fields[form.active].push(value),
                        _ => {}
                    },
                    Page::HostCreate => match key.code {
                        KeyCode::Esc => page = Page::Hosts,
                        KeyCode::Tab | KeyCode::Down => {
                            host_form.active = (host_form.active + 1) % 2
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            host_form.active = (host_form.active + 1) % 2
                        }
                        KeyCode::Enter => {
                            pending_host_notice = Some(last_seen_notice.clone());
                            if command_tx
                                .blocking_send(TuiCommand::CreateHost(host_form.input()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Hosts;
                        }
                        KeyCode::Backspace => {
                            host_form.fields[host_form.active].pop();
                        }
                        KeyCode::Char(value) => host_form.fields[host_form.active].push(value),
                        _ => {}
                    },
                    Page::HostCreateResult => match key.code {
                        KeyCode::Enter | KeyCode::Esc => {
                            dismissed_notice = Some(host_create_result.clone());
                            page = Page::Hosts;
                        }
                        KeyCode::Char('q') => break Ok(()),
                        _ => {}
                    },
                    Page::ProxyNodeCreate => match key.code {
                        KeyCode::Esc => page = Page::Nodes,
                        KeyCode::Tab | KeyCode::Down => proxy_node_form.move_focus(true),
                        KeyCode::BackTab | KeyCode::Up => proxy_node_form.move_focus(false),
                        KeyCode::Left => proxy_node_form.handle_choice(&snapshot, false),
                        KeyCode::Right | KeyCode::Char(' ') => {
                            proxy_node_form.handle_choice(&snapshot, true)
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::CreateProxyNode(proxy_node_form.input()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Nodes;
                        }
                        KeyCode::Backspace => {
                            if let Some(field) = proxy_node_form.editable_field() {
                                proxy_node_form.fields[field].pop();
                            }
                        }
                        KeyCode::Char(value) => {
                            if let Some(field) = proxy_node_form.editable_field() {
                                proxy_node_form.fields[field].push(value);
                            }
                        }
                        _ => {}
                    },
                    Page::Revoke => match key.code {
                        KeyCode::Esc => {
                            page = if revoke_returns_to_nodes {
                                Page::Hosts
                            } else {
                                Page::Dashboard
                            };
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::RevokeNode(revoke_node_id.clone()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = if revoke_returns_to_nodes {
                                Page::Hosts
                            } else {
                                Page::Dashboard
                            };
                        }
                        KeyCode::Backspace => {
                            revoke_node_id.pop();
                        }
                        KeyCode::Char(value) => revoke_node_id.push(value),
                        _ => {}
                    },
                    Page::UserDetail => match key.code {
                        KeyCode::Esc | KeyCode::Left => page = Page::Users,
                        KeyCode::Char('b') | KeyCode::Enter => {
                            selected_binding = 0;
                            page = Page::NicBindings;
                        }
                        KeyCode::Char('e') => {
                            subscription_form = SubscriptionEditFormState::from_snapshot(
                                &snapshot,
                                selected_subscription,
                            );
                            if subscription_form.is_some() {
                                page = Page::SubscriptionEdit;
                            }
                        }
                        KeyCode::Char('n') => {
                            subscription_form = SubscriptionEditFormState::from_snapshot(
                                &snapshot,
                                selected_subscription,
                            );
                            if subscription_form.is_some() {
                                node_assignment = NodeAssignmentState::from_snapshot(
                                    &snapshot,
                                    selected_subscription,
                                )
                                .unwrap_or_default();
                                page = Page::SubscriptionNodes;
                            }
                        }
                        KeyCode::Char('T') => {
                            rotate_kind = RotateKind::Token;
                            page = Page::SubscriptionRotateConfirm;
                        }
                        KeyCode::Char('U') => {
                            rotate_kind = RotateKind::Uuid;
                            page = Page::SubscriptionRotateConfirm;
                        }
                        KeyCode::Up => {
                            selected_subscription = selected_subscription.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if !detail.subscriptions.is_empty() {
                                    selected_subscription = (selected_subscription + 1)
                                        .min(detail.subscriptions.len() - 1);
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if let Some(subscription) =
                                    detail.subscriptions.get(selected_subscription)
                                {
                                    if command_tx
                                        .blocking_send(TuiCommand::ResetSubscription {
                                            user_id: detail.user.id.clone(),
                                            subscription_id: subscription.id.clone(),
                                        })
                                        .is_err()
                                    {
                                        break Ok(());
                                    }
                                }
                            }
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        _ => {}
                    },
                    Page::NicBindings => match key.code {
                        KeyCode::Esc | KeyCode::Left => page = Page::UserDetail,
                        KeyCode::Up => selected_binding = selected_binding.saturating_sub(1),
                        KeyCode::Down => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                let count = detail
                                    .nic_bindings
                                    .iter()
                                    .filter(|binding| {
                                        detail.subscriptions.get(selected_subscription).is_some_and(
                                            |subscription| {
                                                subscription.id == binding.subscription_id
                                            },
                                        )
                                    })
                                    .count();
                                if count > 0 {
                                    selected_binding = (selected_binding + 1).min(count - 1);
                                }
                            }
                        }
                        KeyCode::Char('a') => {
                            let subscription_id =
                                snapshot.user_detail.as_ref().and_then(|detail| {
                                    detail
                                        .subscriptions
                                        .get(selected_subscription)
                                        .map(|subscription| subscription.id.as_str())
                                });
                            nic_form =
                                NicBindingFormState::from_snapshot(&snapshot, subscription_id);
                            page = Page::NicCreate;
                        }
                        KeyCode::Char('D') => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if let Some(binding) = detail
                                    .nic_bindings
                                    .iter()
                                    .filter(|binding| {
                                        detail.subscriptions.get(selected_subscription).is_some_and(
                                            |subscription| {
                                                subscription.id == binding.subscription_id
                                            },
                                        )
                                    })
                                    .nth(selected_binding)
                                {
                                    unbind_binding_id = binding.id.clone();
                                    page = Page::NicUnbindConfirm;
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if let Some(binding) = detail
                                    .nic_bindings
                                    .iter()
                                    .filter(|binding| {
                                        detail.subscriptions.get(selected_subscription).is_some_and(
                                            |subscription| {
                                                subscription.id == binding.subscription_id
                                            },
                                        )
                                    })
                                    .nth(selected_binding)
                                {
                                    if command_tx
                                        .blocking_send(TuiCommand::ResetNicBinding {
                                            user_id: detail.user.id.clone(),
                                            binding_id: binding.id.clone(),
                                        })
                                        .is_err()
                                    {
                                        break Ok(());
                                    }
                                }
                            }
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        _ => {}
                    },
                    Page::NicCreate => match key.code {
                        KeyCode::Esc => page = Page::NicBindings,
                        KeyCode::Tab | KeyCode::Down => nic_form.active = (nic_form.active + 1) % 6,
                        KeyCode::BackTab | KeyCode::Up => {
                            nic_form.active = (nic_form.active + 5) % 6
                        }
                        KeyCode::Enter => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if let Some(subscription) =
                                    detail.subscriptions.get(selected_subscription)
                                {
                                    if command_tx
                                        .blocking_send(TuiCommand::AddNicBinding {
                                            user_id: detail.user.id.clone(),
                                            subscription_id: subscription.id.clone(),
                                            input: nic_form.input(),
                                        })
                                        .is_err()
                                    {
                                        break Ok(());
                                    }
                                }
                            }
                            page = Page::NicBindings;
                        }
                        KeyCode::Backspace => {
                            nic_form.fields[nic_form.active].pop();
                        }
                        KeyCode::Char(value) => nic_form.fields[nic_form.active].push(value),
                        _ => {}
                    },
                    Page::NicUnbindConfirm => match key.code {
                        KeyCode::Esc => page = Page::NicBindings,
                        KeyCode::Enter => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if command_tx
                                    .blocking_send(TuiCommand::UnbindNicBinding {
                                        user_id: detail.user.id.clone(),
                                        binding_id: unbind_binding_id.clone(),
                                    })
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                            page = Page::NicBindings;
                        }
                        _ => {}
                    },
                    Page::SubscriptionEdit => match key.code {
                        KeyCode::Esc => page = Page::UserDetail,
                        KeyCode::Tab | KeyCode::Down => {
                            if let Some(form) = subscription_form.as_mut() {
                                form.active = (form.active + 1) % 7;
                            }
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            if let Some(form) = subscription_form.as_mut() {
                                form.active = (form.active + 6) % 7;
                            }
                        }
                        KeyCode::Enter => {
                            if let (Some(detail), Some(form)) =
                                (snapshot.user_detail.as_ref(), subscription_form.as_ref())
                            {
                                if command_tx
                                    .blocking_send(TuiCommand::UpdateSubscription {
                                        user_id: detail.user.id.clone(),
                                        input: form.input(),
                                    })
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                            page = Page::UserDetail;
                        }
                        KeyCode::Backspace => {
                            if let Some(form) = subscription_form.as_mut() {
                                form.fields[form.active].pop();
                            }
                        }
                        KeyCode::Char(value) => {
                            if let Some(form) = subscription_form.as_mut() {
                                form.fields[form.active].push(value);
                            }
                        }
                        _ => {}
                    },
                    Page::SubscriptionNodes => match key.code {
                        KeyCode::Esc => page = Page::UserDetail,
                        KeyCode::Up => {
                            node_assignment.cursor = node_assignment.cursor.saturating_sub(1);
                        }
                        KeyCode::Down if !snapshot.proxy_nodes.is_empty() => {
                            node_assignment.cursor =
                                (node_assignment.cursor + 1).min(snapshot.proxy_nodes.len() - 1);
                        }
                        KeyCode::Char(' ') => {
                            if let Some(node) = snapshot.proxy_nodes.get(node_assignment.cursor) {
                                node_assignment.toggle(node);
                            }
                        }
                        KeyCode::Enter if !node_assignment.selected_ids.is_empty() => {
                            if let (Some(detail), Some(form)) =
                                (snapshot.user_detail.as_ref(), subscription_form.as_mut())
                            {
                                form.fields[1] = node_assignment.selected_ids.join(",");
                                if command_tx
                                    .blocking_send(TuiCommand::UpdateSubscription {
                                        user_id: detail.user.id.clone(),
                                        input: form.input(),
                                    })
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                            page = Page::UserDetail;
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        _ => {}
                    },
                    Page::SubscriptionRotateConfirm => match key.code {
                        KeyCode::Esc => page = Page::UserDetail,
                        KeyCode::Enter => {
                            if let Some(detail) = snapshot.user_detail.as_ref() {
                                if let Some(subscription) =
                                    detail.subscriptions.get(selected_subscription)
                                {
                                    let command = match rotate_kind {
                                        RotateKind::Token => TuiCommand::RotateSubscriptionToken {
                                            user_id: detail.user.id.clone(),
                                            subscription_id: subscription.id.clone(),
                                        },
                                        RotateKind::Uuid => TuiCommand::RotateSubscriptionUuid {
                                            user_id: detail.user.id.clone(),
                                            subscription_id: subscription.id.clone(),
                                        },
                                    };
                                    if command_tx.blocking_send(command).is_err() {
                                        break Ok(());
                                    }
                                }
                            }
                            page = Page::UserDetail;
                        }
                        _ => {}
                    },
                    Page::Nodes => match key.code {
                        KeyCode::Char('q') => break Ok(()),
                        KeyCode::Esc | KeyCode::Left | KeyCode::Char('1') => page = Page::Dashboard,
                        KeyCode::Char('2') => page = Page::Users,
                        KeyCode::Tab | KeyCode::Char('4') => {
                            selected_node = 0;
                            page = Page::Hosts;
                        }
                        KeyCode::Char('5') => page = Page::Logs,
                        KeyCode::Up => selected_proxy_node = selected_proxy_node.saturating_sub(1),
                        KeyCode::Down if !snapshot.proxy_nodes.is_empty() => {
                            selected_proxy_node =
                                (selected_proxy_node + 1).min(snapshot.proxy_nodes.len() - 1);
                        }
                        KeyCode::Char('a') | KeyCode::Char('n') => {
                            proxy_node_form = ProxyNodeFormState::new(&snapshot);
                            page = Page::ProxyNodeCreate;
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        KeyCode::Char('e') | KeyCode::Enter => {
                            if let Some(node) = snapshot.proxy_nodes.get(selected_proxy_node) {
                                edit_proxy_node_id = node.id.clone();
                                proxy_node_form = ProxyNodeFormState::from_node(node);
                                page = Page::ProxyNodeEdit;
                            }
                        }
                        KeyCode::Char('d') => {
                            if let Some(node) = snapshot.proxy_nodes.get(selected_proxy_node) {
                                let status = if node.status == "active" {
                                    "disabled"
                                } else {
                                    "active"
                                };
                                if command_tx
                                    .blocking_send(TuiCommand::SetProxyNodeStatus {
                                        proxy_node_id: node.id.clone(),
                                        status: status.into(),
                                    })
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                        }
                        KeyCode::Char('D') => {
                            if let Some(node) = snapshot.proxy_nodes.get(selected_proxy_node) {
                                delete_proxy_node_id = node.id.clone();
                                page = Page::ProxyNodeDeleteConfirm;
                            }
                        }
                        _ => {}
                    },
                    Page::Hosts => match key.code {
                        KeyCode::Char('q') => break Ok(()),
                        KeyCode::Esc | KeyCode::Left | KeyCode::Char('1') => page = Page::Dashboard,
                        KeyCode::Char('2') => page = Page::Users,
                        KeyCode::Char('3') => page = Page::Nodes,
                        KeyCode::Tab | KeyCode::Char('5') => page = Page::Logs,
                        KeyCode::Up => selected_node = selected_node.saturating_sub(1),
                        KeyCode::Down if !snapshot.nodes.is_empty() => {
                            selected_node = (selected_node + 1).min(snapshot.nodes.len() - 1);
                        }
                        KeyCode::Char('a') | KeyCode::Char('n') => {
                            host_form = HostFormState::default();
                            page = Page::HostCreate;
                        }
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        KeyCode::Char('e') | KeyCode::Enter => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                edit_host_id = node.id.clone();
                                host_form = HostFormState::from_node(node);
                                page = Page::HostEdit;
                            }
                        }
                        KeyCode::Char('d') => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                let status = if node.management_status == "active" {
                                    "disabled"
                                } else {
                                    "active"
                                };
                                if command_tx
                                    .blocking_send(TuiCommand::SetHostStatus {
                                        node_id: node.id.clone(),
                                        status: status.into(),
                                    })
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                revoke_returns_to_nodes = true;
                                revoke_node_id = node.id.clone();
                                page = Page::Revoke;
                            }
                        }
                        KeyCode::Char('i') => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                pending_host_notice = Some(last_seen_notice.clone());
                                if command_tx
                                    .blocking_send(TuiCommand::ShowAgentInstall(node.id.clone()))
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                        }
                        KeyCode::Char('u') => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                if command_tx
                                    .blocking_send(TuiCommand::ShowAgentUpgrade(node.id.clone()))
                                    .is_err()
                                {
                                    break Ok(());
                                }
                            }
                        }
                        KeyCode::Char('D') => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                delete_host_id = node.id.clone();
                                page = Page::HostDeleteConfirm;
                            }
                        }
                        _ => {}
                    },
                    Page::HostEdit => match key.code {
                        KeyCode::Esc => page = Page::Hosts,
                        KeyCode::Tab | KeyCode::Down => {
                            host_form.active = (host_form.active + 1) % 2;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            host_form.active = (host_form.active + 1) % 2;
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::UpdateHost {
                                    node_id: edit_host_id.clone(),
                                    input: host_form.input(),
                                })
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Hosts;
                        }
                        KeyCode::Backspace => {
                            host_form.fields[host_form.active].pop();
                        }
                        KeyCode::Char(value) => host_form.fields[host_form.active].push(value),
                        _ => {}
                    },
                    Page::HostDeleteConfirm => match key.code {
                        KeyCode::Esc => page = Page::Hosts,
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::DeleteHost(delete_host_id.clone()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Hosts;
                        }
                        _ => {}
                    },
                    Page::ProxyNodeEdit => match key.code {
                        KeyCode::Esc => page = Page::Nodes,
                        KeyCode::Tab | KeyCode::Down => proxy_node_form.move_focus(true),
                        KeyCode::BackTab | KeyCode::Up => proxy_node_form.move_focus(false),
                        KeyCode::Left => proxy_node_form.handle_choice(&snapshot, false),
                        KeyCode::Right | KeyCode::Char(' ') => {
                            proxy_node_form.handle_choice(&snapshot, true)
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::UpdateProxyNode {
                                    proxy_node_id: edit_proxy_node_id.clone(),
                                    input: proxy_node_form.input(),
                                })
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Nodes;
                        }
                        KeyCode::Backspace => {
                            if let Some(field) = proxy_node_form.editable_field() {
                                proxy_node_form.fields[field].pop();
                            }
                        }
                        KeyCode::Char(value) => {
                            if let Some(field) = proxy_node_form.editable_field() {
                                proxy_node_form.fields[field].push(value);
                            }
                        }
                        _ => {}
                    },
                    Page::ProxyNodeDeleteConfirm => match key.code {
                        KeyCode::Esc => page = Page::Nodes,
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::DeleteProxyNode(
                                    delete_proxy_node_id.clone(),
                                ))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Nodes;
                        }
                        _ => {}
                    },
                    Page::Logs => match key.code {
                        KeyCode::Char('q') => break Ok(()),
                        KeyCode::Tab | KeyCode::Esc | KeyCode::Char('1') => page = Page::Dashboard,
                        KeyCode::Char('2') => page = Page::Users,
                        KeyCode::Char('3') => page = Page::Nodes,
                        KeyCode::Char('4') => page = Page::Hosts,
                        KeyCode::Char('R') => match command_tx.blocking_send(TuiCommand::Refresh) {
                            Ok(()) => {}
                            Err(_) => break Ok(()),
                        },
                        _ => {}
                    },
                }
            }
        }
    };
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn draw_primary_shell(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    selected_tab: usize,
) -> Rect {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let header = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(18), Constraint::Length(34)])
        .split(areas[0]);
    let tabs = Tabs::new(vec![
        "仪表盘 [1]",
        "用户 [2]",
        "节点 [3]",
        "主机 [4]",
        "日志 [5]",
    ])
    .select(selected_tab)
    .divider(Span::styled(" │ ", Style::default().fg(Color::DarkGray)))
    .style(Style::default().fg(Color::DarkGray))
    .highlight_style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Green)),
    );
    frame.render_widget(tabs, header[0]);
    frame.render_widget(
        Paragraph::new(format!(
            "Xenon {}  Agent {}/{}",
            env!("CARGO_PKG_VERSION"),
            snapshot.connected_agents,
            snapshot.nodes.len()
        ))
        .alignment(Alignment::Right)
        .style(Style::default().fg(Color::DarkGray))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::Green)),
        ),
        header[1],
    );

    let (footer, footer_style) = if snapshot.notice.is_empty() {
        let keys = match selected_tab {
            0 => "[1-5/Tab] 切换  [a] 新建用户  [R] 刷新  [q] 退出",
            1 => "[↑↓] 选择  [Enter] 详情  [a] 新建用户  [R] 刷新  [3] 节点  [4] 主机",
            2 => "[↑↓] 选择  [Enter/e] 编辑  [a] 新建  [d] 启停  [D] 删除  [R] 刷新",
            3 => "[↑↓] 选择  [a] 新建  [i] 安装命令  [e] 编辑  [d] 启停  [u] 升级  [R] 刷新",
            _ => "[1-4/Tab] 切换页面  [R] 刷新  [q] 退出",
        };
        (keys, Style::default().fg(Color::DarkGray))
    } else {
        (
            snapshot.notice.as_str(),
            Style::default().fg(notice_color(&snapshot.notice)),
        )
    };
    frame.render_widget(Paragraph::new(footer).style(footer_style), areas[2]);
    areas[1]
}

fn draw_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &TuiSnapshot) {
    if area.width < 48 || area.height < 12 {
        let text = format!(
            "Panel: active  Agents: {}  Users: {}  Nodes: {}",
            snapshot.connected_agents,
            snapshot.users.len(),
            snapshot.nodes.len()
        );
        frame.render_widget(Paragraph::new(text).block(panel_block("Xenon 状态")), area);
        return;
    }

    let rate_height = if area.height >= 30 { 7 } else { 0 };
    let stats_height = if area.height >= 22 { 3 } else { 0 };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(rate_height),
            Constraint::Length(stats_height),
            Constraint::Min(6),
            Constraint::Length(8),
        ])
        .split(area);
    let online_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| node.node_status == "online")
        .count();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● Panel ", Style::default().fg(Color::Green)),
            Span::styled("运行中", Style::default().fg(Color::Green)),
            Span::raw("    Agent "),
            Span::styled(
                snapshot.connected_agents.to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("    在线主机 "),
            Span::styled(online_nodes.to_string(), Style::default().fg(Color::Cyan)),
            Span::raw("    最近事件 "),
            Span::styled(
                truncate(snapshot.last_agent_event.as_deref().unwrap_or("-"), 54),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(panel_block("Xenon 状态")),
        areas[0],
    );

    frame.render_widget(
        Paragraph::new(system_metrics_line(snapshot)).block(panel_block("主机资源")),
        areas[1],
    );

    if rate_height > 0 {
        let charts = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[2]);
        let down_title = format!("↓ 下行  {}/s", format_bytes(snapshot.down_rate_bps as i64));
        frame.render_widget(
            Sparkline::default()
                .data(&snapshot.down_rate_history)
                .style(Style::default().fg(Color::Blue))
                .block(panel_block(&down_title)),
            charts[0],
        );
        let up_title = format!("↑ 上行  {}/s", format_bytes(snapshot.up_rate_bps as i64));
        frame.render_widget(
            Sparkline::default()
                .data(&snapshot.up_rate_history)
                .style(Style::default().fg(Color::Magenta))
                .block(panel_block(&up_title)),
            charts[1],
        );
    }

    if stats_height > 0 {
        frame.render_widget(
            Paragraph::new(user_stats_line(snapshot)).block(panel_block("用户摘要")),
            areas[3],
        );
    }

    let max_usage = snapshot
        .users
        .iter()
        .map(|user| user.charged_bytes)
        .max()
        .unwrap_or_default();
    let user_rows = snapshot.users.iter().take(5).map(|user| {
        Row::new(vec![
            Cell::from(user.username.clone()),
            Cell::from(user.status.clone()).style(Style::default().fg(status_color(&user.status))),
            Cell::from(user.subscription_count.to_string()),
            Cell::from(format_bytes(user.charged_bytes)),
            Cell::from(quota_label(user)),
            Cell::from(usage_bar(user.charged_bytes, max_usage, 18)),
        ])
    });
    let users_title = format!("用量 Top 5 · 共 {} 用户", snapshot.users.len());
    let users = Table::new(
        user_rows,
        [
            Constraint::Percentage(22),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Length(16),
            Constraint::Min(12),
        ],
    )
    .header(table_header([
        "用户",
        "状态",
        "订阅",
        "Xray 计费",
        "额度",
        "相对用量",
    ]))
    .column_spacing(1)
    .block(panel_block(&users_title));
    frame.render_widget(users, areas[4]);

    let node_rows = snapshot.proxy_nodes.iter().take(5).map(|node| {
        Row::new(vec![
            Cell::from(node.name.clone()),
            Cell::from(node.host_name.clone()),
            Cell::from(format!("{}-{}", node.protocol, node.transport)),
            Cell::from(node.security.clone()),
            Cell::from(proxy_published_endpoint(node)),
            Cell::from(node.status.clone()).style(Style::default().fg(status_color(&node.status))),
        ])
    });
    let nodes_title = format!("节点摘要 · {}", snapshot.proxy_nodes.len());
    let nodes = Table::new(
        node_rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(18),
            Constraint::Length(15),
            Constraint::Length(10),
            Constraint::Min(18),
            Constraint::Length(9),
        ],
    )
    .header(table_header([
        "节点",
        "主机",
        "协议",
        "安全",
        "发布地址",
        "状态",
    ]))
    .column_spacing(1)
    .block(panel_block(&nodes_title));
    frame.render_widget(nodes, areas[5]);
}

fn quota_label(user: &models::UserSummary) -> String {
    match user.traffic_limit_bytes {
        Some(limit) if limit > 0 => {
            let percent = user.charged_bytes as f64 / limit as f64 * 100.0;
            format!("{} {:.1}%", format_bytes(limit), percent)
        }
        _ => "∞".to_string(),
    }
}

struct UserStats {
    enabled: usize,
    over_quota: usize,
    expired: usize,
    total_charged: i64,
}

fn user_stats(snapshot: &TuiSnapshot) -> UserStats {
    UserStats {
        enabled: snapshot
            .users
            .iter()
            .filter(|user| user.status == "active")
            .count(),
        over_quota: snapshot
            .users
            .iter()
            .filter(|user| {
                user.traffic_limit_bytes
                    .is_some_and(|limit| limit > 0 && user.charged_bytes >= limit)
            })
            .count(),
        expired: snapshot
            .users
            .iter()
            .filter(|user| user.expired_subscriptions > 0)
            .count(),
        total_charged: snapshot
            .users
            .iter()
            .map(|user| user.charged_bytes)
            .fold(0_i64, i64::saturating_add),
    }
}

fn user_stats_line(snapshot: &TuiSnapshot) -> Line<'static> {
    let stats = user_stats(snapshot);
    Line::from(vec![
        Span::raw("用户 "),
        Span::styled(
            snapshot.users.len().to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  启用:"),
        Span::styled(stats.enabled.to_string(), Style::default().fg(Color::Green)),
        Span::raw("  超额:"),
        Span::styled(
            stats.over_quota.to_string(),
            Style::default().fg(if stats.over_quota > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw("  到期:"),
        Span::styled(
            stats.expired.to_string(),
            Style::default().fg(if stats.expired > 0 {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw("    本周期计费 "),
        Span::styled(
            format_bytes(stats.total_charged),
            Style::default().fg(Color::Cyan),
        ),
    ])
}

fn system_metrics_line(snapshot: &TuiSnapshot) -> Line<'static> {
    let cpu_values = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.cpu_usage_basis_points)
        .collect::<Vec<_>>();
    let cpu = if cpu_values.is_empty() {
        "-".into()
    } else {
        format!(
            "{:.1}%",
            cpu_values.iter().sum::<i64>() as f64 / cpu_values.len() as f64 / 100.0
        )
    };
    let loads = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.load_1_milli)
        .collect::<Vec<_>>();
    let load = if loads.is_empty() {
        "-".into()
    } else {
        format!(
            "{:.2}",
            loads.iter().sum::<i64>() as f64 / loads.len() as f64 / 1000.0
        )
    };
    let (memory_used, memory_total) = aggregate_resource(snapshot, ResourceKind::Memory);
    let (disk_used, disk_total) = aggregate_resource(snapshot, ResourceKind::Disk);
    Line::from(vec![
        Span::raw("主机 "),
        Span::styled(
            snapshot.nodes.len().to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  在线 "),
        Span::styled(
            snapshot
                .nodes
                .iter()
                .filter(|node| node.node_status == "online")
                .count()
                .to_string(),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  CPU "),
        Span::styled(cpu, Style::default().fg(Color::Cyan)),
        Span::raw("  内存 "),
        Span::styled(
            resource_label(memory_used, memory_total),
            Style::default().fg(Color::LightBlue),
        ),
        Span::raw("  磁盘 "),
        Span::styled(
            resource_label(disk_used, disk_total),
            Style::default().fg(Color::Magenta),
        ),
        Span::raw("  负载 "),
        Span::styled(load, Style::default().fg(Color::Yellow)),
    ])
}

fn proxy_published_endpoint(node: &models::ProxyNodeRecord) -> String {
    format!(
        "{}:{}",
        node.publish_host.as_deref().unwrap_or(&node.landing_host),
        node.publish_port.unwrap_or(node.listen_port)
    )
}

fn draw_proxy_nodes(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &TuiSnapshot,
    selected: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(6)])
        .split(area);
    let rows = snapshot
        .proxy_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            Row::new(vec![
                Cell::from(node.name.clone()),
                Cell::from(node.host_name.clone()),
                Cell::from(format!("{}-{}", node.protocol, node.transport)),
                Cell::from(node.security.clone()),
                Cell::from(node.listen_port.to_string()),
                Cell::from(proxy_published_endpoint(node)),
                Cell::from(node.status.clone())
                    .style(Style::default().fg(status_color(&node.status))),
            ])
            .style(selected_style(index == selected))
        });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(18),
                Constraint::Percentage(17),
                Constraint::Length(16),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Min(18),
                Constraint::Length(9),
            ],
        )
        .header(table_header([
            "节点",
            "主机",
            "协议",
            "安全",
            "端口",
            "发布地址",
            "状态",
        ]))
        .column_spacing(1)
        .block(panel_block(&format!(
            "Xray 节点 · {}",
            snapshot.proxy_nodes.len()
        ))),
        areas[0],
    );
    let detail = snapshot.proxy_nodes.get(selected).map_or_else(
        || "尚未创建协议节点".to_string(),
        |node| {
            format!(
                "ID: {}  主机: {} ({})\n协议: {}  传输: {}  安全: {}  监听: {}\nServer Name: {}  WS Path: {}\nReality Public Key: {}",
                node.id,
                node.host_name,
                node.host_id,
                node.protocol,
                node.transport,
                node.security,
                node.listen_port,
                node.server_name.as_deref().unwrap_or("-"),
                node.websocket_path.as_deref().unwrap_or("-"),
                node.reality_public_key.as_deref().unwrap_or("-"),
            )
        },
    );
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(panel_block("选中节点")),
        areas[1],
    );
}

fn draw_logs(frame: &mut ratatui::Frame<'_>, area: Rect, snapshot: &TuiSnapshot) {
    let lines = if snapshot.agent_events.is_empty() {
        vec![Line::from(Span::styled(
            "暂无 Agent 事件",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        snapshot
            .agent_events
            .iter()
            .rev()
            .map(|event| Line::from(event.clone()))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel_block("Agent 事件日志 · 最新在前")),
        area,
    );
}

fn draw_users(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &TuiSnapshot,
    selected_user: usize,
) {
    if area.width < 48 || area.height < 8 {
        frame.render_widget(
            Paragraph::new(format!("用户总数: {}", snapshot.users.len()))
                .block(panel_block("用户管理")),
            area,
        );
        return;
    }
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);
    frame.render_widget(
        Paragraph::new(user_stats_line(snapshot)).block(panel_block("用户摘要")),
        areas[0],
    );
    let max_usage = snapshot
        .users
        .iter()
        .map(|user| user.charged_bytes)
        .max()
        .unwrap_or_default();
    let rows = snapshot.users.iter().enumerate().map(|(index, user)| {
        Row::new(vec![
            Cell::from(user.username.clone()),
            Cell::from(user.status.clone()).style(Style::default().fg(status_color(&user.status))),
            Cell::from(user.subscription_count.to_string()),
            Cell::from(format_bytes(user.charged_bytes)),
            Cell::from(quota_label(user)),
            Cell::from(usage_bar(user.charged_bytes, max_usage, 18)),
        ])
        .style(selected_style(index == selected_user))
    });
    let title = format!("用户列表 · {}", snapshot.users.len());
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(22),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Length(16),
            Constraint::Min(12),
        ],
    )
    .header(table_header([
        "用户",
        "状态",
        "订阅",
        "Xray 计费",
        "额度",
        "相对用量",
    ]))
    .column_spacing(1)
    .block(panel_block(&title));
    frame.render_widget(table, areas[1]);
}

fn draw_nodes(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &TuiSnapshot,
    selected_node: usize,
) {
    if area.width < 48 || area.height < 10 {
        frame.render_widget(
            Paragraph::new(format!("主机总数: {}", snapshot.nodes.len()))
                .block(panel_block("主机管理")),
            area,
        );
        return;
    }

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(7),
            Constraint::Length(7),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(system_metrics_line(snapshot)).block(panel_block("主机资源")),
        areas[0],
    );

    let rows = snapshot.nodes.iter().enumerate().map(|(index, node)| {
        let nic = snapshot.host_nic.iter().find(|nic| nic.node_id == node.id);
        Row::new(vec![
            Cell::from(node.name.clone()),
            Cell::from(node.management_status.clone())
                .style(Style::default().fg(status_color(&node.management_status))),
            Cell::from(node.node_status.clone())
                .style(Style::default().fg(status_color(&node.node_status))),
            Cell::from(node.agent_status.clone().unwrap_or_else(|| "未注册".into())),
            Cell::from(node.landing_host.clone()),
            Cell::from(nic.map_or_else(
                || "-".into(),
                |nic| {
                    format!(
                        "↓{}/s ↑{}/s",
                        format_bytes(nic.down_rate_bps as i64),
                        format_bytes(nic.up_rate_bps as i64)
                    )
                },
            )),
            Cell::from(nic.map_or_else(
                || "-".into(),
                |nic| {
                    format!(
                        "↓{} ↑{}",
                        format_bytes(nic.rx_bytes),
                        format_bytes(nic.tx_bytes)
                    )
                },
            )),
        ])
        .style(selected_style(index == selected_node))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(18),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Min(18),
            Constraint::Length(23),
            Constraint::Length(23),
        ],
    )
    .header(table_header([
        "主机",
        "管理",
        "运行",
        "Agent",
        "主机地址",
        "网卡实时",
        "网卡累计",
    ]))
    .column_spacing(1)
    .block(panel_block("主机清单"));
    frame.render_widget(table, areas[1]);

    let detail = snapshot.nodes.get(selected_node).map_or_else(
        || "尚未添加主机".to_string(),
        |node| {
            let nic_line = snapshot
                .host_nic
                .iter()
                .find(|nic| nic.node_id == node.id)
                .map_or_else(
                    || "暂无网卡数据".into(),
                    |nic| {
                        format!(
                            "网卡 RX {}  TX {}  采样 {}",
                            format_bytes(nic.rx_bytes),
                            format_bytes(nic.tx_bytes),
                            nic.sampled_at
                        )
                    },
                );
            format!(
                "ID: {}\n版本: Agent {}  Xray {}  配置修订 {}\n负载: {}  磁盘: {}\n地址: {}  {}",
                node.id,
                node.agent_version.as_deref().unwrap_or("-"),
                node.xray_version.as_deref().unwrap_or("-"),
                node.desired_revision,
                node.load_1_milli.map_or_else(
                    || "-".into(),
                    |value| format!("{:.2}", value as f64 / 1000.0)
                ),
                resource_label(
                    node.disk_used_bytes.unwrap_or_default(),
                    node.disk_total_bytes.unwrap_or_default()
                ),
                node.landing_host,
                nic_line,
            )
        },
    );
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(panel_block("选中主机")),
        areas[2],
    );
}

#[derive(Clone, Copy)]
enum ResourceKind {
    Memory,
    Disk,
}

fn aggregate_resource(snapshot: &TuiSnapshot, kind: ResourceKind) -> (i64, i64) {
    snapshot.nodes.iter().fold((0_i64, 0_i64), |totals, node| {
        let values = match kind {
            ResourceKind::Memory => (node.memory_used_bytes, node.memory_total_bytes),
            ResourceKind::Disk => (node.disk_used_bytes, node.disk_total_bytes),
        };
        match values {
            (Some(used), Some(total)) if total > 0 => (
                totals.0.saturating_add(used.max(0)),
                totals.1.saturating_add(total),
            ),
            _ => totals,
        }
    })
}

fn panel_block<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(Span::styled(title, Style::default().fg(Color::Green)))
}

fn table_header<const N: usize>(labels: [&str; N]) -> Row<'static> {
    Row::new(labels.map(|label| Cell::from(label.to_string()))).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
}

fn selected_style(selected: bool) -> Style {
    if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "active" | "online" | "running" | "ready" | "connected" => Color::Green,
        "disabled" | "offline" | "failed" | "revoked" => Color::Red,
        _ => Color::Yellow,
    }
}

fn notice_color(notice: &str) -> Color {
    let lower = notice.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") || lower.contains("cannot") {
        Color::Red
    } else {
        Color::Yellow
    }
}

fn resource_label(used: i64, total: i64) -> String {
    if total > 0 {
        format!("{} / {}", format_bytes(used), format_bytes(total))
    } else {
        "暂无数据".into()
    }
}

fn usage_bar(value: i64, max: i64, width: usize) -> String {
    let filled = if max > 0 {
        ((value.max(0) as u128 * width as u128) / max as u128) as usize
    } else {
        0
    }
    .min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn draw_host_edit(frame: &mut ratatui::Frame<'_>, form: &HostFormState, node_id: &str) {
    draw_host_form(
        frame,
        form,
        &format!("编辑主机 {}", truncate(node_id, 16)),
        "保存",
    );
}

fn draw_host_delete_confirm(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, node_id: &str) {
    let body = vec![
        Line::from(format!("逻辑删除主机 {node_id}？")),
        Line::default(),
        Line::from("需先移除该主机节点的活跃订阅和网卡绑定。"),
        Line::from("该主机的 Agent 证书将被吊销。"),
        Line::default(),
        Line::from(Span::styled(
            snapshot.notice.clone(),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let inner = modal_area(frame, 64, 10, "删除主机", true);
    modal_body_and_hint(frame, inner, body, "[Enter] 确认  [Esc] 取消");
}

fn draw_revoke(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, node_id: &str) {
    let mut body = vec![
        Line::from(vec![
            Span::raw("主机 ID: "),
            Span::styled(
                node_id.to_string(),
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            "已注册主机:",
            Style::default().fg(Color::Yellow),
        )),
    ];
    for node in snapshot.nodes.iter().take(8) {
        body.push(Line::from(format!("  {} ({})", node.id, node.name)));
    }
    let inner = modal_area(frame, 72, 16, "紧急吊销 Agent 证书", true);
    modal_body_and_hint(
        frame,
        inner,
        body,
        "[Enter] 吊销该节点全部 Agent 证书  [Esc] 取消",
    );
}

fn draw_user_detail(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    selected_subscription: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let Some(detail) = snapshot.user_detail.as_ref() else {
        frame.render_widget(
            Paragraph::new("Loading user detail...")
                .block(Block::default().borders(Borders::ALL).title("User")),
            areas[1],
        );
        frame.render_widget(
            Paragraph::new("Esc Back")
                .style(Style::default().fg(Color::Green))
                .block(Block::default().borders(Borders::ALL).title("Keys")),
            areas[2],
        );
        return;
    };
    frame.render_widget(
        Paragraph::new(format!(
            "User: {}   current Xray billed: {}",
            detail.user.username,
            format_bytes(detail.user.charged_bytes)
        ))
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("User detail")),
        areas[0],
    );
    let mut body = String::new();
    if detail.subscriptions.is_empty() {
        body.push_str("No subscriptions\n");
    }
    for (index, subscription) in detail.subscriptions.iter().enumerate() {
        let charged = detail
            .node_usage
            .iter()
            .filter(|usage| usage.subscription_id == subscription.id)
            .map(|usage| usage.charged_bytes)
            .fold(0_i64, i64::saturating_add);
        let nic = detail
            .nic_usage
            .iter()
            .find(|usage| usage.subscription_id == subscription.id)
            .map(|usage| {
                format!(
                    "  NIC header used/limit={}/{} (excluded from Xray total)",
                    format_bytes(usage.used_bytes),
                    format_bytes(usage.limit_bytes)
                )
            })
            .unwrap_or_default();
        body.push_str(&format!(
            "{} Subscription {}: {}  xray billed={}  multiplier={}x  cycle={}..{}\n",
            if index == selected_subscription {
                ">"
            } else {
                " "
            },
            truncate(&subscription.id, 12),
            truncate(&subscription.name, 24),
            format_bytes(charged),
            subscription.traffic_multiplier_basis_points as f64 / 10_000.0,
            subscription.current_cycle_start,
            subscription
                .current_cycle_end
                .map_or_else(|| "-".into(), |value| value.to_string()),
        ));
        if !nic.is_empty() {
            body.push_str(&format!("{nic}\n"));
        }
        for usage in detail
            .node_usage
            .iter()
            .filter(|usage| usage.subscription_id == subscription.id)
        {
            body.push_str(&format!(
                "    node {:<18} up={} down={} billed={}\n",
                truncate(&usage.node_name, 18),
                format_bytes(usage.uplink_bytes),
                format_bytes(usage.downlink_bytes),
                format_bytes(usage.charged_bytes),
            ));
        }
    }
    if !snapshot.notice.is_empty() {
        body.push_str(&format!("\n{}\n", snapshot.notice));
    }
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Subscriptions"),
        ),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(
            "↑↓ 选择  n 分配节点  e 编辑  b 网卡  r 重置流量  R 刷新  T Token  U UUID  Esc 返回",
        )
        .style(Style::default().fg(Color::Green))
        .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_node_assignment(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    selected_subscription: usize,
    state: &NodeAssignmentState,
) {
    let subscription_name = snapshot
        .user_detail
        .as_ref()
        .and_then(|detail| detail.subscriptions.get(selected_subscription))
        .map_or("-", |subscription| subscription.name.as_str());
    let visible_rows = 12_usize;
    let start = state.cursor.saturating_sub(visible_rows.saturating_sub(1));
    let mut body = vec![
        Line::from(vec![
            Span::styled("订阅  ", Style::default().fg(Color::Yellow)),
            Span::styled(
                truncate(subscription_name, 36),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::default(),
    ];
    if snapshot.proxy_nodes.is_empty() {
        body.push(Line::from("暂无节点，请先在节点页按 a 创建。"));
    } else {
        body.push(Line::from(Span::styled(
            "    节点                     协议              主机               状态",
            Style::default().fg(Color::DarkGray),
        )));
        for (index, node) in snapshot
            .proxy_nodes
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_rows)
        {
            let checked = state.contains(&node.id);
            let selected = index == state.cursor;
            let marker = if checked { "[x]" } else { "[ ]" };
            let protocol = format!("{}/{}/{}", node.protocol, node.transport, node.security);
            let line = format!(
                "{} {:<24} {:<17} {:<18} {}",
                marker,
                truncate(&node.name, 24),
                truncate(&protocol, 17),
                truncate(&node.host_name, 18),
                node.status,
            );
            body.push(Line::from(Span::styled(
                line,
                if selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else if node.status != "active" {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                },
            )));
        }
    }
    body.push(Line::default());
    body.push(Line::from(Span::styled(
        format!("已选择 {} 个节点", state.selected_ids.len()),
        Style::default().fg(if state.selected_ids.is_empty() {
            Color::Red
        } else {
            Color::Green
        }),
    )));
    let inner = modal_area(
        frame,
        92,
        (body.len() as u16 + 3).min(22),
        "分配订阅节点",
        false,
    );
    modal_body_and_hint(
        frame,
        inner,
        body,
        "[↑↓] 选择  [Space] 勾选  [Enter] 保存  [R] 刷新  [Esc] 取消",
    );
}

fn draw_nic_bindings(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    selected_subscription: usize,
    selected_binding: usize,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let subscription = snapshot
        .user_detail
        .as_ref()
        .and_then(|detail| detail.subscriptions.get(selected_subscription));
    frame.render_widget(
        Paragraph::new(format!(
            "NIC bindings: {}",
            subscription.map_or("-", |record| record.name.as_str())
        ))
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("NIC billing")),
        areas[0],
    );
    let mut body = String::new();
    if let (Some(detail), Some(subscription)) = (snapshot.user_detail.as_ref(), subscription) {
        let bindings = detail
            .nic_bindings
            .iter()
            .filter(|binding| binding.subscription_id == subscription.id)
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            body.push_str("No active NIC bindings\n");
        }
        for (index, binding) in bindings.iter().enumerate() {
            body.push_str(&format!(
                "{} {}  {}/{}  direction={}  used/limit={}/{}\n",
                if index == selected_binding { ">" } else { " " },
                truncate(&binding.id, 12),
                truncate(&binding.node_id, 12),
                binding.interface_name,
                binding.billing_direction,
                format_bytes(binding.used_bytes),
                format_bytes(binding.traffic_limit_bytes),
            ));
            body.push_str(&format!(
                "    reset={}  cycle={}..{}  initial={}\n",
                binding.reset_policy,
                binding.current_cycle_start,
                binding
                    .current_cycle_end
                    .map_or_else(|| "-".into(), |value| value.to_string()),
                format_bytes(binding.initial_used_bytes),
            ));
        }
    }
    if !snapshot.notice.is_empty() {
        body.push_str(&format!("\n{}\n", snapshot.notice));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Bindings")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("↑↓ 选择  a 添加  r 重置周期  R 刷新  D 解绑  Esc 返回")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_nic_create(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    form: &NicBindingFormState,
) {
    let labels = [
        "节点 ID",
        "网卡名",
        "流量额度字节",
        "初始已用字节",
        "计费方向 (rx_tx/tx_only/rx_only)",
        "重置策略",
    ];
    let mut body = form_lines(&labels, &form.fields, form.active);
    body.push(Line::default());
    body.push(Line::from(Span::styled(
        "已上报网卡:",
        Style::default().fg(Color::Yellow),
    )));
    for interface in snapshot.interfaces.iter().take(6) {
        body.push(Line::from(format!(
            "  {}/{}",
            interface.node_id, interface.interface_name
        )));
    }
    let inner = modal_area(frame, 72, 18, "添加网卡绑定", false);
    modal_body_and_hint(
        frame,
        inner,
        body,
        "[Tab/↑↓] 切换  [Enter] 添加  [Esc] 取消",
    );
}

fn draw_nic_unbind_confirm(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    binding_id: &str,
) {
    let body = vec![
        Line::from(format!("解绑网卡绑定 {binding_id}？")),
        Line::default(),
        Line::from("历史记录仍会保留。"),
        Line::default(),
        Line::from(Span::styled(
            snapshot.notice.clone(),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let inner = modal_area(frame, 64, 9, "确认解绑", true);
    modal_body_and_hint(frame, inner, body, "[Enter] 确认  [Esc] 取消");
}

fn draw_subscription_edit(frame: &mut ratatui::Frame<'_>, form: &SubscriptionEditFormState) {
    let labels = [
        "订阅名称",
        "节点 ID (逗号分隔)",
        "Xray 流量额度字节 (留空不限)",
        "到期 Unix 时间 (留空永久)",
        "计费倍率 (1 或 2)",
        "重置 (never/manual/daily:HH:MM/monthly:DAY@HH:MM/interval:DAYS)",
        "状态 (active 或 disabled)",
    ];
    let body = form_lines(&labels, &form.fields, form.active);
    let inner = modal_area(
        frame,
        76,
        12,
        &format!("编辑订阅 {}", truncate(&form.subscription_id, 16)),
        false,
    );
    modal_body_and_hint(
        frame,
        inner,
        body,
        "[Tab/↑↓] 切换  [Enter] 保存  [Esc] 取消",
    );
}

fn draw_subscription_rotate_confirm(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    selected_subscription: usize,
    kind: RotateKind,
) {
    let subscription = snapshot
        .user_detail
        .as_ref()
        .and_then(|detail| detail.subscriptions.get(selected_subscription));
    let (label, impact) = match kind {
        RotateKind::Token => ("订阅 Token", "旧订阅链接将立即失效。"),
        RotateKind::Uuid => ("Xray UUID", "Agent 将替换该用户，客户端需刷新订阅。"),
    };
    let body = vec![
        Line::from(format!(
            "确认为 {} 轮换{label}？",
            subscription.map_or("-", |record| record.name.as_str())
        )),
        Line::default(),
        Line::from(impact),
        Line::from("新密钥只显示一次。"),
    ];
    let inner = modal_area(frame, 64, 9, "确认轮换", true);
    modal_body_and_hint(frame, inner, body, "[Enter] 确认  [Esc] 取消");
}

fn draw_create(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, form: &FormState) {
    let labels = [
        "用户名 *必填",
        "订阅名称 *必填",
        "节点 ID (逗号分隔)",
        "Xray 流量额度字节 (留空不限)",
        "到期 Unix 时间 (留空永久)",
        "计费倍率 (1 或 2)",
        "重置 (never/manual/daily:HH:MM/monthly:DAY@HH:MM/interval:DAYS)",
        "网卡 节点/网卡/额度/初始[/方向[/重置]] (; 分隔)",
    ];
    let mut body = form_lines(&labels, &form.fields, form.active);
    body.push(Line::default());
    body.push(Line::from(Span::styled(
        "可用节点:",
        Style::default().fg(Color::Yellow),
    )));
    for node in snapshot
        .proxy_nodes
        .iter()
        .filter(|node| node.status == "active")
        .take(5)
    {
        body.push(Line::from(format!(
            "  {} ({} / {})",
            node.id, node.name, node.host_name
        )));
    }
    body.push(Line::from(Span::styled(
        "已上报网卡:",
        Style::default().fg(Color::Yellow),
    )));
    for interface in snapshot.interfaces.iter().take(5) {
        body.push(Line::from(format!(
            "  {}/{}",
            interface.node_id, interface.interface_name
        )));
    }
    let inner = modal_area(frame, 78, 24, "新建用户与订阅", false);
    modal_body_and_hint(
        frame,
        inner,
        body,
        "[Tab/↑↓] 切换  [Enter] 创建  [Esc] 取消",
    );
}

fn draw_host_create(frame: &mut ratatui::Frame<'_>, form: &HostFormState) {
    draw_host_form(frame, form, "添加主机", "创建并生成 Agent 安装命令");
}

fn draw_host_create_result(frame: &mut ratatui::Frame<'_>, result: &str) {
    let command_result = result.split_once("; install: ");
    let disabled_result = result.split_once("; installer-disabled; ");
    let summary = command_result
        .map(|(summary, _)| summary)
        .or_else(|| disabled_result.map(|(summary, _)| summary))
        .unwrap_or(result);
    let mut body = vec![
        Line::from(Span::styled(
            summary.to_string(),
            Style::default().fg(if result.starts_with("operation failed:") {
                Color::Red
            } else {
                Color::Green
            }),
        )),
        Line::default(),
    ];
    if let Some((_, command)) = command_result {
        body.push(Line::from(Span::styled(
            "Agent 一键安装命令（包含一次性注册 Token）：",
            Style::default().fg(Color::Yellow),
        )));
        body.push(Line::default());
        body.push(Line::from(command.to_string()));
    } else if let Some((_, details)) = disabled_result {
        let (token, panel) = details
            .split_once("; panel: ")
            .map(|(token, panel)| (token.trim_start_matches("token: "), panel))
            .unwrap_or(("-", "-"));
        body.push(Line::from(Span::styled(
            "无法生成一键安装命令",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        body.push(Line::default());
        body.push(Line::from(
            "当前配置未启用 [agent_install]；本地 127.0.0.1 地址也不能供远程 VPS 使用。",
        ));
        body.push(Line::from(
            "正式部署请使用 scripts/install-panel.sh，或补全 xenon.toml 的 [agent_install] 后重启 Panel。",
        ));
        body.push(Line::default());
        body.push(Line::from(vec![
            Span::styled(
                "手工注册 Token（仅显示一次）：",
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(token.to_string()),
        ]));
        body.push(Line::from(format!("Panel 当前监听地址：{panel}")));
    }
    let inner = modal_area(
        frame,
        100,
        20,
        "主机创建结果",
        result.starts_with("operation failed:"),
    );
    modal_body_and_hint(frame, inner, body, "[Enter/Esc] 关闭  [q] 退出");
}

fn draw_host_form(frame: &mut ratatui::Frame<'_>, form: &HostFormState, title: &str, action: &str) {
    let labels = ["主机名称 *必填", "主机地址/IP *必填"];
    let body = form_lines(&labels, &form.fields, form.active);
    let inner = modal_area(frame, 68, 8, title, false);
    modal_body_and_hint(
        frame,
        inner,
        body,
        &format!("[Tab/↑↓] 切换  [Enter] {action}  [Esc] 取消"),
    );
}

fn draw_proxy_node_create(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    form: &ProxyNodeFormState,
) {
    draw_proxy_node_form(frame, snapshot, form, "添加 Xray 节点", "创建");
}

fn draw_proxy_node_edit(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    form: &ProxyNodeFormState,
    node_id: &str,
) {
    draw_proxy_node_form(
        frame,
        snapshot,
        form,
        &format!("编辑 Xray 节点 {}", truncate(node_id, 16)),
        "保存",
    );
}

fn draw_proxy_node_form(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    form: &ProxyNodeFormState,
    title: &str,
    action: &str,
) {
    let mut body = vec![Line::default()];
    for (position, item) in form.visible_items().iter().copied().enumerate() {
        let selected = position == form.active;
        let (label, value, is_choice) = match item {
            ProxyNodeFormItem::Tag => ("Tag *必填", form.fields[1].clone(), false),
            ProxyNodeFormItem::Protocol => (
                "协议 (←/→ 切换)",
                format!("◀ {} ▶", PROXY_NODE_PROFILES[form.profile]),
                true,
            ),
            ProxyNodeFormItem::Host => {
                let host = snapshot.nodes.iter().find(|host| host.id == form.fields[0]);
                (
                    "主机 (←/→ 切换)",
                    host.map_or_else(
                        || "◀ 暂无主机 ▶".into(),
                        |host| {
                            format!(
                                "◀ {} · {} ▶",
                                truncate(&host.name, 18),
                                truncate(&host.landing_host, 24)
                            )
                        },
                    ),
                    true,
                )
            }
            ProxyNodeFormItem::Port => ("端口 *必填 (默认 443)", form.fields[2].clone(), false),
            ProxyNodeFormItem::ServerName => {
                ("server_name (SNI) *必填", form.fields[5].clone(), false)
            }
            ProxyNodeFormItem::WebSocketPath => ("path *必填", form.fields[6].clone(), false),
        };
        let cursor = if selected && !is_choice { "_" } else { "" };
        body.push(Line::from(vec![
            Span::styled(
                format!(" {:<27}", label),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(" {value}{cursor}  "),
                if selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]));
        body.push(Line::default());
    }
    body.push(Line::from(Span::styled(
        match form.profile {
            0 => "  UUID 自动生成；Reality 凭据由 Agent 管理，不在表单中显示",
            1 => "  UUID 自动生成；只需填写节点连接参数",
            2 => "  UUID 自动生成；WebSocket 节点只需额外填写 path",
            _ => "  Shadowsocks 密钥由 Agent 管理，无 SNI / path 字段",
        },
        Style::default().fg(Color::DarkGray),
    )));
    let height = (body.len() as u16 + 3).min(20);
    let inner = modal_area(frame, 82, height, title, false);
    modal_body_and_hint(
        frame,
        inner,
        body,
        &format!("[Tab/↑↓] 选择  [←/→/Space] 切换选项  [Enter] {action}  [Esc] 取消"),
    );
}

fn draw_proxy_node_delete_confirm(frame: &mut ratatui::Frame<'_>, node_id: &str) {
    let body = vec![
        Line::from(format!("逻辑删除 Xray 节点 {node_id}？")),
        Line::default(),
        Line::from("存在活跃订阅分配时不会删除。"),
    ];
    let inner = modal_area(frame, 64, 8, "删除 Xray 节点", true);
    modal_body_and_hint(frame, inner, body, "[Enter] 确认  [Esc] 取消");
}

fn modal_area(
    frame: &mut ratatui::Frame<'_>,
    width: u16,
    height: u16,
    title: &str,
    danger: bool,
) -> Rect {
    let screen = frame.area();
    let width = width.min(screen.width.saturating_sub(2)).max(1);
    let height = height.min(screen.height.saturating_sub(2)).max(1);
    let x = screen.x + (screen.width.saturating_sub(width)) / 2;
    let y = screen.y + (screen.height.saturating_sub(height)) / 2;
    let area = Rect::new(x, y, width, height);
    frame.render_widget(Clear, area);
    let border = if danger { Color::Red } else { Color::Green };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn modal_body_and_hint(
    frame: &mut ratatui::Frame<'_>,
    inner: Rect,
    body: Vec<Line<'static>>,
    hint: &str,
) {
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), rows[0]);
    frame.render_widget(
        Paragraph::new(hint.to_string()).style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );
}

fn form_lines(labels: &[&str], values: &[String], active: usize) -> Vec<Line<'static>> {
    labels
        .iter()
        .zip(values)
        .enumerate()
        .map(|(index, (label, value))| {
            if index == active {
                Line::from(format!("> {label}: {value}"))
                    .style(Style::default().fg(Color::Black).bg(Color::Cyan))
            } else {
                Line::from(vec![
                    Span::styled("  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(label.to_string(), Style::default().fg(Color::Yellow)),
                    Span::styled(": ", Style::default().fg(Color::DarkGray)),
                    Span::styled(value.clone(), Style::default().fg(Color::White)),
                ])
            }
        })
        .collect()
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn format_reset_policy(policy: &str, anchor: Option<i64>) -> String {
    let format_time = |seconds: i64| {
        let seconds = seconds.clamp(0, 86_399);
        format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
    };
    match policy {
        "daily" => anchor
            .map(|seconds| format!("daily:{}", format_time(seconds)))
            .unwrap_or_else(|| "never".into()),
        "monthly" => anchor
            .map(|encoded| {
                format!(
                    "monthly:{}@{}",
                    encoded / 86_400,
                    format_time(encoded % 86_400)
                )
            })
            .unwrap_or_else(|| "never".into()),
        value if value.starts_with("interval_days:") => {
            format!("interval:{}", &value[14..])
        }
        "manual" => "manual".into(),
        _ => "never".into(),
    }
}

fn format_bytes(value: i64) -> String {
    let mut amount = value.max(0) as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if amount < 1024.0 || unit == "TiB" {
            return format!("{amount:.1}{unit}");
        }
        amount /= 1024.0;
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::{
        agent_upgrade_command, create_host, create_host_registration, create_proxy_node,
        create_subscription, format_bytes, format_reset_policy, CreateHostInput,
        CreateSubscriptionInput, FormState, HostFormState, NicBindingFormState, NicRateTracker,
        NodeAssignmentState, ProxyNodeFormItem, ProxyNodeFormState, ProxyNodeInput,
        SubscriptionEditFormState, TuiSnapshot,
    };
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use uuid::Uuid;
    use xenon_storage::{models, Database};

    #[test]
    fn nic_rate_tracker_computes_rates_and_rebaselines_on_counter_reset() {
        let mut tracker = NicRateTracker::default();
        tracker.observe(Some(models::NicCounterTotals {
            rx_bytes: 1_000,
            tx_bytes: 500,
            sampled_at: 100,
        }));
        assert_eq!(tracker.down_rate_bps, 0);
        tracker.observe(Some(models::NicCounterTotals {
            rx_bytes: 21_000,
            tx_bytes: 10_500,
            sampled_at: 110,
        }));
        assert_eq!(tracker.down_rate_bps, 2_000);
        assert_eq!(tracker.up_rate_bps, 1_000);
        // Same sample again: rate holds, history still advances.
        tracker.observe(Some(models::NicCounterTotals {
            rx_bytes: 21_000,
            tx_bytes: 10_500,
            sampled_at: 110,
        }));
        assert_eq!(tracker.down_rate_bps, 2_000);
        assert_eq!(tracker.down_history.len(), 3);
        // Counter reset (agent reboot) must not produce a negative spike.
        tracker.observe(Some(models::NicCounterTotals {
            rx_bytes: 40,
            tx_bytes: 20,
            sampled_at: 120,
        }));
        assert_eq!(tracker.down_rate_bps, 0);
        assert_eq!(tracker.up_rate_bps, 0);
        tracker.observe(Some(models::NicCounterTotals {
            rx_bytes: 1_040,
            tx_bytes: 520,
            sampled_at: 130,
        }));
        assert_eq!(tracker.down_rate_bps, 100);
        assert_eq!(tracker.up_rate_bps, 50);
    }

    #[test]
    fn renders_every_page_in_a_small_terminal_without_panicking() {
        let snapshot = TuiSnapshot::default();
        let form = FormState {
            fields: Default::default(),
            active: 0,
        };
        let host_form = HostFormState::default();
        let proxy_node_form = ProxyNodeFormState::new(&snapshot);
        let nic_form = NicBindingFormState::from_snapshot(&snapshot, None);
        let subscription_form = SubscriptionEditFormState {
            fields: Default::default(),
            subscription_id: "subscription".into(),
            starts_at: 0,
            current_cycle_start: 0,
            active: 0,
        };
        let node_assignment = NodeAssignmentState::default();
        let mut terminal = Terminal::new(TestBackend::new(24, 4)).expect("test terminal");
        terminal
            .draw(|frame| {
                let dashboard = super::draw_primary_shell(frame, &snapshot, 0);
                super::draw_dashboard(frame, dashboard, &snapshot);
                let users = super::draw_primary_shell(frame, &snapshot, 1);
                super::draw_users(frame, users, &snapshot, 0);
                let nodes = super::draw_primary_shell(frame, &snapshot, 2);
                super::draw_proxy_nodes(frame, nodes, &snapshot, 0);
                let hosts = super::draw_primary_shell(frame, &snapshot, 3);
                super::draw_nodes(frame, hosts, &snapshot, 0);
                let logs = super::draw_primary_shell(frame, &snapshot, 4);
                super::draw_logs(frame, logs, &snapshot);
                super::draw_host_edit(frame, &host_form, "host");
                super::draw_host_delete_confirm(frame, &snapshot, "host");
                super::draw_proxy_node_edit(frame, &snapshot, &proxy_node_form, "node");
                super::draw_proxy_node_delete_confirm(frame, "node");
                super::draw_revoke(frame, &snapshot, "node");
                super::draw_user_detail(frame, &snapshot, 0);
                super::draw_nic_bindings(frame, &snapshot, 0, 0);
                super::draw_nic_create(frame, &snapshot, &nic_form);
                super::draw_nic_unbind_confirm(frame, &snapshot, "binding");
                super::draw_subscription_edit(frame, &subscription_form);
                super::draw_node_assignment(frame, &snapshot, 0, &node_assignment);
                super::draw_subscription_rotate_confirm(
                    frame,
                    &snapshot,
                    0,
                    super::RotateKind::Token,
                );
                super::draw_create(frame, &snapshot, &form);
                super::draw_host_create(frame, &host_form);
                super::draw_host_create_result(
                    frame,
                    "created host host-a; install: curl -fsSL https://example.test | sudo bash",
                );
                super::draw_proxy_node_create(frame, &snapshot, &proxy_node_form);
            })
            .expect("small terminal render");
    }

    #[test]
    fn dashboard_and_nodes_render_structured_operational_data() {
        let snapshot = TuiSnapshot {
            connected_agents: 1,
            last_agent_event: Some("node-a connected".into()),
            users: vec![models::UserSummary {
                id: "user-a".into(),
                username: "admin".into(),
                display_name: None,
                status: "active".into(),
                subscription_count: 2,
                charged_bytes: 8 * 1024 * 1024,
                traffic_limit_bytes: Some(500 * 1024 * 1024 * 1024),
                expired_subscriptions: 0,
            }],
            nodes: vec![models::NodeOverview {
                id: "node-a".into(),
                name: "Tokyo".into(),
                landing_host: "203.0.113.10".into(),
                xray_listen_port: 443,
                publish_host: Some("relay.example".into()),
                publish_port: Some(8443),
                protocol: "vless".into(),
                transport: "tcp".into(),
                security: "reality".into(),
                server_name: Some("example.com".into()),
                reality_public_key: Some("public-key".into()),
                reality_short_id: Some("short-id".into()),
                reality_fingerprint: Some("chrome".into()),
                node_status: "online".into(),
                management_status: "active".into(),
                desired_revision: 3,
                agent_status: Some("online".into()),
                last_seen_at: Some(1),
                agent_version: Some("0.1.0-alpha.5".into()),
                xray_version: Some("26.6.27".into()),
                cpu_usage_basis_points: Some(1250),
                load_1_milli: Some(420),
                memory_total_bytes: Some(1024 * 1024 * 1024),
                memory_used_bytes: Some(512 * 1024 * 1024),
                disk_total_bytes: Some(20 * 1024 * 1024 * 1024),
                disk_used_bytes: Some(5 * 1024 * 1024 * 1024),
            }],
            proxy_nodes: vec![models::ProxyNodeRecord {
                id: "proxy-a".into(),
                host_id: "node-a".into(),
                name: "Tokyo Reality".into(),
                host_name: "Tokyo".into(),
                landing_host: "203.0.113.10".into(),
                listen_port: 443,
                publish_host: Some("relay.example".into()),
                publish_port: Some(8443),
                protocol: "vless".into(),
                transport: "tcp".into(),
                security: "reality".into(),
                server_name: Some("example.com".into()),
                websocket_path: None,
                vless_encryption: None,
                reality_public_key: Some("public-key".into()),
                reality_short_id: Some("short-id".into()),
                reality_fingerprint: Some("chrome".into()),
                status: "active".into(),
            }],
            host_nic: vec![super::HostNicSnapshot {
                node_id: "node-a".into(),
                rx_bytes: 8 * 1024 * 1024,
                tx_bytes: 2 * 1024 * 1024,
                down_rate_bps: 2048,
                up_rate_bps: 1024,
                sampled_at: 100,
            }],
            ..TuiSnapshot::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).expect("test terminal");

        terminal
            .draw(|frame| {
                let area = super::draw_primary_shell(frame, &snapshot, 0);
                super::draw_dashboard(frame, area, &snapshot);
            })
            .expect("dashboard render");
        let dashboard = rendered_text(&terminal);
        let compact_dashboard = dashboard.replace(' ', "");
        assert!(compact_dashboard.contains("仪表盘[1]"), "{dashboard}");
        assert!(compact_dashboard.contains("CPU12.5%"), "{dashboard}");
        assert!(compact_dashboard.contains("用量Top5"), "{dashboard}");
        assert!(compact_dashboard.contains("用户摘要"), "{dashboard}");
        assert!(dashboard.contains("admin"), "{dashboard}");
        assert!(dashboard.contains("relay.example:8443"), "{dashboard}");

        let mut users_terminal = Terminal::new(TestBackend::new(120, 36)).expect("test terminal");
        users_terminal
            .draw(|frame| {
                let area = super::draw_primary_shell(frame, &snapshot, 1);
                super::draw_users(frame, area, &snapshot, 0);
            })
            .expect("users render");
        let users = rendered_text(&users_terminal);
        let compact_users = users.replace(' ', "");
        assert!(compact_users.contains("用户列表"), "{users}");
        assert!(users.contains("admin"), "{users}");
        assert!(users_terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.bg == Color::Cyan));

        let mut node_terminal = Terminal::new(TestBackend::new(120, 36)).expect("test terminal");
        node_terminal
            .draw(|frame| {
                let area = super::draw_primary_shell(frame, &snapshot, 2);
                super::draw_proxy_nodes(frame, area, &snapshot, 0);
            })
            .expect("nodes render");
        let nodes = rendered_text(&node_terminal);
        let compact_nodes = nodes.replace(' ', "");
        assert!(compact_nodes.contains("节点[3]"), "{nodes}");
        assert!(compact_nodes.contains("Xray节点"), "{nodes}");
        assert!(compact_nodes.contains("选中节点"), "{nodes}");
        assert!(nodes.contains("Tokyo Reality"), "{nodes}");

        let mut host_terminal = Terminal::new(TestBackend::new(140, 36)).expect("test terminal");
        host_terminal
            .draw(|frame| {
                let area = super::draw_primary_shell(frame, &snapshot, 3);
                super::draw_nodes(frame, area, &snapshot, 0);
            })
            .expect("hosts render");
        let hosts = rendered_text(&host_terminal);
        let compact_hosts = hosts.replace(' ', "");
        assert!(compact_hosts.contains("主机资源"), "{hosts}");
        assert!(compact_hosts.contains("网卡实时"), "{hosts}");
        assert!(compact_hosts.contains("↓2.0KiB/s↑1.0KiB/s"), "{hosts}");
        assert!(compact_hosts.contains("↓8.0MiB↑2.0MiB"), "{hosts}");
    }

    #[test]
    fn host_creation_result_shows_the_full_agent_install_command() {
        let result = "created host host-a; install: curl -fsSL --proto '=https' \
                      https://downloads.example/install-agent.sh | sudo bash -s -- \
                      --panel 'https://panel.example:50051' --node 'host-a' --token 'secret'";
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test terminal");
        terminal
            .draw(|frame| super::draw_host_create_result(frame, result))
            .expect("result render");
        let rendered = rendered_text(&terminal);
        assert!(rendered.contains("created host host-a"), "{rendered}");
        assert!(rendered.contains("install-agent.sh"), "{rendered}");
        assert!(rendered.contains("--token 'secret'"), "{rendered}");
    }

    #[test]
    fn missing_agent_installer_is_explained_without_repeating_the_token() {
        let result = "已为主机 host-a 签发新的 Agent 注册凭据; installer-disabled; \
                      token: one-time-secret; panel: 127.0.0.1:50051";
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test terminal");
        terminal
            .draw(|frame| super::draw_host_create_result(frame, result))
            .expect("result render");
        let rendered = rendered_text(&terminal);
        let compact = rendered.replace(' ', "");
        assert!(compact.contains("无法生成一键安装命令"), "{rendered}");
        assert!(rendered.contains("[agent_install]"), "{rendered}");
        assert_eq!(rendered.matches("one-time-secret").count(), 1, "{rendered}");
    }

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn creates_user_and_subscription_from_wizard_input() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        database
            .ensure_development_node("node-a", 1)
            .await
            .expect("node");
        database
            .insert_interface_snapshots("node-a", "boot-a", 1, 1, &[("eth0".into(), 10, 20)])
            .await
            .expect("interface");

        let notice = create_subscription(
            &database,
            "http://127.0.0.1:18181",
            CreateSubscriptionInput {
                username: "alice".into(),
                name: "Alice default".into(),
                node_ids: "node-a".into(),
                limit_bytes: "1024".into(),
                expires_at: String::new(),
                multiplier: "2".into(),
                reset_policy: "daily:00:00".into(),
                nic_bindings: "node-a/eth0/2048/12/tx_only/monthly:31@00:00".into(),
            },
        )
        .await
        .expect("create subscription");

        assert!(notice.contains("http://127.0.0.1:18181/sub/"));
        let users = database.list_user_summaries(0).await.expect("users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "alice");
        let subscriptions = database
            .list_user_subscriptions(&users[0].id)
            .await
            .expect("subscriptions");
        Uuid::parse_str(&subscriptions[0].xray_uuid).expect("generated Xray UUID");
        assert_eq!(subscriptions[0].traffic_multiplier_basis_points, 20_000);
        let binding = sqlx::query_as::<_, (String, String, i64, String)>(
            "SELECT billing_direction, reset_policy, initial_used_bytes, interface_name
             FROM nic_bindings WHERE subscription_id = ?",
        )
        .bind(&subscriptions[0].id)
        .fetch_one(database.pool())
        .await
        .expect("NIC binding");
        assert_eq!(
            binding,
            ("tx_only".into(), "monthly".into(), 12, "eth0".into())
        );
    }

    #[test]
    fn formats_binary_traffic_units() {
        assert_eq!(format_bytes(1024), "1.0KiB");
        assert_eq!(format_bytes(-1), "0.0B");
        assert_eq!(format_reset_policy("daily", Some(3600)), "daily:01:00");
        assert_eq!(
            format_reset_policy("monthly", Some(31 * 86_400 + 4500)),
            "monthly:31@01:15"
        );
        assert_eq!(
            format_reset_policy("interval_days:7", Some(10)),
            "interval:7"
        );
    }

    #[test]
    fn proxy_node_form_only_exposes_fields_for_the_selected_protocol() {
        let snapshot = TuiSnapshot::default();
        let mut form = ProxyNodeFormState::new(&snapshot);
        assert_eq!(
            form.visible_items(),
            &[
                ProxyNodeFormItem::Tag,
                ProxyNodeFormItem::Protocol,
                ProxyNodeFormItem::Host,
                ProxyNodeFormItem::Port,
                ProxyNodeFormItem::ServerName,
            ]
        );

        form.cycle_profile(true);
        assert_eq!(form.visible_items().len(), 4);
        form.cycle_profile(true);
        assert_eq!(
            form.visible_items().last(),
            Some(&ProxyNodeFormItem::WebSocketPath)
        );
        form.cycle_profile(true);
        assert_eq!(form.visible_items().len(), 4);
    }

    #[test]
    fn proxy_node_protocol_changes_only_while_its_row_is_focused() {
        let snapshot = TuiSnapshot::default();
        let mut form = ProxyNodeFormState::new(&snapshot);
        form.handle_choice(&snapshot, true);
        assert_eq!(form.profile, 0);

        form.move_focus(true);
        assert_eq!(form.active_item(), ProxyNodeFormItem::Protocol);
        form.handle_choice(&snapshot, true);
        assert_eq!(form.profile, 1);
    }

    #[test]
    fn proxy_node_form_generates_hidden_reality_short_id() {
        let form = ProxyNodeFormState::new(&TuiSnapshot::default());
        let input = form.input();
        assert_eq!(input.reality_short_id.len(), 8);
        assert!(input
            .reality_short_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(input.reality_fingerprint, "chrome");
    }

    #[test]
    fn proxy_node_form_keeps_credential_and_publish_fields_out_of_view() {
        let snapshot = TuiSnapshot {
            nodes: vec![models::NodeOverview {
                id: "host-a".into(),
                name: "Tokyo".into(),
                landing_host: "203.0.113.10".into(),
                xray_listen_port: 443,
                publish_host: None,
                publish_port: None,
                protocol: "vless".into(),
                transport: "tcp".into(),
                security: "none".into(),
                server_name: None,
                reality_public_key: None,
                reality_short_id: None,
                reality_fingerprint: None,
                node_status: "pending".into(),
                management_status: "active".into(),
                desired_revision: 0,
                agent_status: None,
                last_seen_at: None,
                agent_version: None,
                xray_version: None,
                cpu_usage_basis_points: None,
                load_1_milli: None,
                memory_total_bytes: None,
                memory_used_bytes: None,
                disk_total_bytes: None,
                disk_used_bytes: None,
            }],
            ..TuiSnapshot::default()
        };
        let form = ProxyNodeFormState::new(&snapshot);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test terminal");
        terminal
            .draw(|frame| super::draw_proxy_node_create(frame, &snapshot, &form))
            .expect("form render");
        let rendered = rendered_text(&terminal);
        let compact = rendered.replace(' ', "");
        assert!(compact.contains("协议(←/→切换)"), "{rendered}");
        assert!(rendered.contains("vless-reality"), "{rendered}");
        assert!(!compact.contains("订阅发布地址"), "{rendered}");
        assert!(!compact.contains("Reality公钥"), "{rendered}");
        assert!(!rendered.contains("short_id"), "{rendered}");
    }

    #[test]
    fn node_assignment_only_adds_active_nodes() {
        let mut state = NodeAssignmentState::default();
        let mut node = models::ProxyNodeRecord {
            id: "proxy-a".into(),
            host_id: "host-a".into(),
            name: "Reality".into(),
            host_name: "Host A".into(),
            landing_host: "203.0.113.10".into(),
            listen_port: 443,
            publish_host: None,
            publish_port: None,
            protocol: "vless".into(),
            transport: "tcp".into(),
            security: "reality".into(),
            server_name: Some("example.com".into()),
            websocket_path: None,
            vless_encryption: None,
            reality_public_key: None,
            reality_short_id: None,
            reality_fingerprint: None,
            status: "disabled".into(),
        };
        state.toggle(&node);
        assert!(state.selected_ids.is_empty());

        node.status = "active".into();
        state.toggle(&node);
        assert!(state.contains("proxy-a"));
        state.toggle(&node);
        assert!(!state.contains("proxy-a"));
    }

    #[tokio::test]
    async fn creates_host_and_registration_token_without_a_protocol_node() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        let install = crate::config::AgentInstallConfig {
            enabled: true,
            script_url: "https://downloads.test/install-agent.sh".into(),
            binary_url: "https://downloads.test/xenon-agent-linux-{arch}".into(),
            binary_sha256: String::new(),
            binary_sha256_x86_64: "a".repeat(64),
            binary_sha256_aarch64: "b".repeat(64),
            binary_version: "0.1.0".into(),
            ca_url: "https://downloads.test/panel-ca.crt".into(),
            ca_path: String::new(),
            panel_endpoint: "https://panel.test:50051".into(),
            enrollment_endpoint: "https://panel.test:50052".into(),
            server_name: "panel.test".into(),
        };
        let notice = create_host(
            &database,
            "127.0.0.1:50051",
            &install,
            CreateHostInput {
                name: "Node A".into(),
                landing_host: "203.0.113.10".into(),
            },
        )
        .await
        .expect("create node");
        assert!(notice.contains("--panel 'https://panel.test:50051'"));
        assert!(notice.contains("--binary-sha256"));
        assert!(notice.contains("--agent-version '0.1.0'"));
        assert!(!notice.contains("example.invalid"));
        let upgrade = agent_upgrade_command(&install, "node-a").expect("upgrade command");
        assert!(upgrade.contains("--upgrade"));
        assert!(upgrade.contains("--rollback"));
        assert!(upgrade.contains("--agent-version '0.1.0'"));
        assert_eq!(database.list_nodes().await.expect("nodes").len(), 1);
        assert!(database
            .list_proxy_nodes()
            .await
            .expect("proxy nodes")
            .is_empty());
        let token_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM registration_tokens")
            .fetch_one(database.pool())
            .await
            .expect("tokens");
        assert_eq!(token_count, 1);

        let regenerated = create_host_registration(
            &database,
            "127.0.0.1:50051",
            &install,
            &database.list_nodes().await.expect("nodes")[0].id,
        )
        .await
        .expect("regenerate install command");
        assert!(regenerated.contains("已为主机"));
        assert!(regenerated.contains("--token '"));
        let token_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM registration_tokens")
            .fetch_one(database.pool())
            .await
            .expect("tokens");
        assert_eq!(token_count, 2);
    }

    #[tokio::test]
    async fn creates_protocol_node_on_an_existing_host() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        database
            .ensure_development_node("host-a", 1)
            .await
            .expect("host");

        let notice = create_proxy_node(
            &database,
            ProxyNodeInput {
                host_id: "host-a".into(),
                name: "WS 8443".into(),
                profile: "vless-ws".into(),
                listen_port: "8443".into(),
                publish_host: "edge.example.com".into(),
                publish_port: "443".into(),
                server_name: String::new(),
                websocket_path: "/xray".into(),
                vless_encryption: String::new(),
                reality_public_key: String::new(),
                reality_short_id: String::new(),
                reality_fingerprint: "chrome".into(),
            },
        )
        .await
        .expect("create protocol node");

        assert!(notice.contains("Agent multi-inbound deployment is pending"));
        let nodes = database.list_proxy_nodes().await.expect("proxy nodes");
        let node = nodes
            .iter()
            .find(|node| node.name == "WS 8443")
            .expect("created protocol node");
        assert_eq!(node.host_id, "host-a");
        assert_eq!(node.transport, "ws");
        assert_eq!(node.websocket_path.as_deref(), Some("/xray"));
        assert_eq!(node.status, "disabled");
    }
}
