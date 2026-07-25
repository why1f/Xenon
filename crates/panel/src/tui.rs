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
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, RwLock};
use uuid::Uuid;
use xenon_storage::{models, Database};

#[derive(Debug, Clone, Default)]
struct TuiSnapshot {
    connected_agents: usize,
    last_agent_event: Option<String>,
    users: Vec<models::UserSummary>,
    nodes: Vec<models::NodeOverview>,
    interfaces: Vec<models::InterfaceRecord>,
    user_detail: Option<models::UserDetail>,
    notice: String,
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
struct CreateNodeInput {
    name: String,
    landing_host: String,
    xray_port: String,
    publish_host: String,
    publish_port: String,
    security: String,
    server_name: String,
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
    CreateSubscription(CreateSubscriptionInput),
    CreateNode(CreateNodeInput),
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
    UpdateNode {
        node_id: String,
        input: CreateNodeInput,
    },
    SetNodeStatus {
        node_id: String,
        status: String,
    },
    ShowAgentUpgrade(String),
    DeleteNode(String),
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

    loop {
        tokio::select! {
            result = &mut tui_task => {
                result.context("join TUI task")??;
                break;
            }
            _ = refresh.tick() => {
                let snapshot = load_snapshot(&state, &database, String::new(), user_detail.clone()).await?;
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    break;
                };
                let result = match command {
                    TuiCommand::CreateSubscription(input) => create_subscription(&database, &subscription_base_url, input).await,
                    TuiCommand::CreateNode(input) => create_node(&database, &grpc_addr, &agent_install, input).await,
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
                    TuiCommand::UpdateNode { node_id, input } => {
                        update_node(&database, &node_id, input).await
                    }
                    TuiCommand::SetNodeStatus { node_id, status } => {
                        set_node_status(&database, &node_id, &status).await
                    }
                    TuiCommand::ShowAgentUpgrade(node_id) => {
                        agent_upgrade_command(&agent_install, &node_id)
                    }
                    TuiCommand::DeleteNode(node_id) => delete_node(&database, &node_id).await,
                };
                let notice = match result {
                    Ok(message) => message,
                    Err(error) => format!("operation failed: {error}"),
                };
                let snapshot = load_snapshot(&state, &database, notice, user_detail.clone()).await?;
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
        anyhow::bail!("node ID is required");
    }
    let revoked = database
        .revoke_node_certificates(node_id, Utc::now().timestamp())
        .await?;
    if revoked == 0 {
        anyhow::bail!("node has no active Agent certificate");
    }
    Ok(format!(
        "revoked {revoked} Agent certificate(s) for node {node_id}"
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

async fn update_node(
    database: &Database,
    node_id: &str,
    input: CreateNodeInput,
) -> anyhow::Result<String> {
    let xray_listen_port = input
        .xray_port
        .trim()
        .parse::<i64>()
        .context("invalid Xray port")?;
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
    let optional = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_string());
    if !database
        .update_node(
            node_id,
            &models::UpdateNode {
                name: input.name.trim().to_string(),
                landing_host: input.landing_host.trim().to_string(),
                xray_listen_port,
                publish_host,
                publish_port,
                security: input.security.trim().to_ascii_lowercase(),
                server_name: optional(input.server_name),
                reality_public_key: optional(input.reality_public_key),
                reality_short_id: optional(input.reality_short_id),
                reality_fingerprint: optional(input.reality_fingerprint),
                updated_at: Utc::now().timestamp(),
            },
        )
        .await?
    {
        anyhow::bail!("node no longer exists");
    }
    Ok(format!("updated node {node_id}"))
}

async fn set_node_status(
    database: &Database,
    node_id: &str,
    status: &str,
) -> anyhow::Result<String> {
    if !database
        .set_node_management_status(node_id, status, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("node no longer exists");
    }
    Ok(format!("node {node_id} is now {status}"))
}

async fn delete_node(database: &Database, node_id: &str) -> anyhow::Result<String> {
    if !database
        .delete_node(node_id, Utc::now().timestamp())
        .await?
    {
        anyhow::bail!("node no longer exists");
    }
    Ok(format!("logically deleted node {node_id}"))
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
    let (connected_agents, last_agent_event) = {
        let runtime = state.read().await;
        (runtime.connected_agents, runtime.last_agent_event.clone())
    };
    Ok(TuiSnapshot {
        connected_agents,
        last_agent_event,
        users: database.list_user_summaries().await?,
        nodes: database.list_node_overviews().await?,
        interfaces: database.list_recent_interfaces().await?,
        user_detail,
        notice,
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

async fn create_node(
    database: &Database,
    grpc_addr: &str,
    agent_install: &AgentInstallConfig,
    input: CreateNodeInput,
) -> anyhow::Result<String> {
    let name = input.name.trim().to_string();
    let landing_host = input.landing_host.trim().to_string();
    let xray_port = input
        .xray_port
        .trim()
        .parse::<i64>()
        .context("invalid Xray port")?;
    let publish_host = input.publish_host.trim();
    let publish_port = input.publish_port.trim();
    let publish_host = (!publish_host.is_empty()).then(|| publish_host.to_string());
    let publish_port = if publish_port.is_empty() {
        None
    } else {
        Some(
            publish_port
                .parse::<i64>()
                .context("invalid publish port")?,
        )
    };
    if publish_host.is_some() != publish_port.is_some() {
        anyhow::bail!("publish host and port must be entered together");
    }
    let security = if input.security.trim().is_empty() {
        "none".to_string()
    } else {
        input.security.trim().to_ascii_lowercase()
    };
    let optional = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_string());
    let server_name = optional(input.server_name);
    let reality_public_key = optional(input.reality_public_key);
    let reality_short_id = optional(input.reality_short_id);
    let reality_fingerprint = optional(input.reality_fingerprint);
    let now = Utc::now().timestamp();
    let node_id = Uuid::now_v7().to_string();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    database
        .create_node_with_registration(
            &models::NewNode {
                id: node_id.clone(),
                name,
                landing_host,
                xray_listen_port: xray_port,
                publish_host,
                publish_port,
                protocol: "vless".into(),
                transport: "tcp".into(),
                security,
                server_name,
                reality_public_key,
                reality_short_id,
                reality_fingerprint,
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
        .context("store node and registration token")?;
    if agent_install.enabled {
        Ok(format!(
            "created node {node_id}; install: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo sh -s -- --panel '{}' --enrollment '{}' --server-name '{}' --node '{}' --token '{}' --binary-url '{}' --binary-sha256 '{}' --agent-version '{}' --ca-url '{}'",
            agent_install.script_url,
            agent_install.panel_endpoint,
            agent_install.enrollment_endpoint,
            agent_install.server_name,
            node_id,
            token,
            agent_install.binary_url,
            agent_install.binary_sha256,
            agent_install.binary_version,
            agent_install.ca_url,
        ))
    } else {
        Ok(format!(
            "created node {node_id}; registration token: {token}; Panel {grpc_addr}; agent installer is not configured"
        ))
    }
}

fn agent_upgrade_command(
    agent_install: &AgentInstallConfig,
    node_id: &str,
) -> anyhow::Result<String> {
    if !agent_install.enabled {
        anyhow::bail!("Agent release source is not configured");
    }
    Ok(format!(
        "node {node_id}; upgrade: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo sh -s -- --upgrade --binary-url '{}' --binary-sha256 '{}' --agent-version '{}'; rollback: curl -fsSL --proto '=https' --tlsv1.2 '{}' | sudo sh -s -- --rollback",
        agent_install.script_url,
        agent_install.binary_url,
        agent_install.binary_sha256,
        agent_install.binary_version,
        agent_install.script_url,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Create,
    NodeCreate,
    Revoke,
    UserDetail,
    NicBindings,
    NicCreate,
    NicUnbindConfirm,
    SubscriptionEdit,
    SubscriptionRotateConfirm,
    Nodes,
    NodeEdit,
    NodeDeleteConfirm,
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

struct NodeFormState {
    fields: [String; 10],
    active: usize,
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

impl SubscriptionEditFormState {
    fn from_snapshot(snapshot: &TuiSnapshot, selected_subscription: usize) -> Option<Self> {
        let detail = snapshot.user_detail.as_ref()?;
        let subscription = detail.subscriptions.get(selected_subscription)?;
        let node_ids = detail
            .node_usage
            .iter()
            .filter(|usage| usage.subscription_id == subscription.id)
            .map(|usage| usage.node_id.as_str())
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

impl Default for NodeFormState {
    fn default() -> Self {
        Self {
            fields: [
                String::new(),
                String::new(),
                "443".into(),
                String::new(),
                String::new(),
                "none".into(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
            active: 0,
        }
    }
}

impl NodeFormState {
    fn from_node(node: &models::NodeOverview) -> Self {
        Self {
            fields: [
                node.name.clone(),
                node.landing_host.clone(),
                node.xray_listen_port.to_string(),
                node.publish_host.clone().unwrap_or_default(),
                node.publish_port
                    .map_or_else(String::new, |port| port.to_string()),
                node.security.clone(),
                node.server_name.clone().unwrap_or_default(),
                node.reality_public_key.clone().unwrap_or_default(),
                node.reality_short_id.clone().unwrap_or_default(),
                node.reality_fingerprint.clone().unwrap_or_default(),
            ],
            active: 0,
        }
    }

    fn input(&self) -> CreateNodeInput {
        CreateNodeInput {
            name: self.fields[0].clone(),
            landing_host: self.fields[1].clone(),
            xray_port: self.fields[2].clone(),
            publish_host: self.fields[3].clone(),
            publish_port: self.fields[4].clone(),
            security: self.fields[5].clone(),
            server_name: self.fields[6].clone(),
            reality_public_key: self.fields[7].clone(),
            reality_short_id: self.fields[8].clone(),
            reality_fingerprint: self.fields[9].clone(),
        }
    }
}

impl FormState {
    fn from_snapshot(snapshot: &TuiSnapshot) -> Self {
        let node_ids = snapshot
            .nodes
            .iter()
            .filter(|node| node.management_status == "active")
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
    let mut node_form = NodeFormState::default();
    let mut nic_form = NicBindingFormState::from_snapshot(&snapshot_rx.borrow(), None);
    let mut subscription_form = None;
    let mut rotate_kind = RotateKind::Token;
    let mut revoke_node_id = String::new();
    let mut revoke_returns_to_nodes = false;
    let mut selected_user = 0_usize;
    let mut selected_subscription = 0_usize;
    let mut selected_binding = 0_usize;
    let mut selected_node = 0_usize;
    let mut edit_node_id = String::new();
    let mut delete_node_id = String::new();
    let mut unbind_binding_id = String::new();
    let result = loop {
        let snapshot = snapshot_rx.borrow().clone();
        selected_user = selected_user.min(snapshot.users.len().saturating_sub(1));
        if let Some(detail) = snapshot.user_detail.as_ref() {
            selected_subscription =
                selected_subscription.min(detail.subscriptions.len().saturating_sub(1));
        }
        selected_node = selected_node.min(snapshot.nodes.len().saturating_sub(1));
        terminal.draw(|frame| match page {
            Page::Dashboard => draw_dashboard(frame, &snapshot, selected_user),
            Page::Create => draw_create(frame, &snapshot, &form),
            Page::NodeCreate => draw_node_create(frame, &node_form),
            Page::Revoke => draw_revoke(frame, &snapshot, &revoke_node_id),
            Page::UserDetail => draw_user_detail(frame, &snapshot, selected_subscription),
            Page::NicBindings => {
                draw_nic_bindings(frame, &snapshot, selected_subscription, selected_binding)
            }
            Page::NicCreate => draw_nic_create(frame, &snapshot, &nic_form),
            Page::NicUnbindConfirm => draw_nic_unbind_confirm(frame, &snapshot, &unbind_binding_id),
            Page::SubscriptionEdit => {
                if let Some(form) = subscription_form.as_ref() {
                    draw_subscription_edit(frame, form);
                }
            }
            Page::SubscriptionRotateConfirm => draw_subscription_rotate_confirm(
                frame,
                &snapshot,
                selected_subscription,
                rotate_kind,
            ),
            Page::Nodes => draw_nodes(frame, &snapshot, selected_node),
            Page::NodeEdit => draw_node_edit(frame, &node_form, &edit_node_id),
            Page::NodeDeleteConfirm => draw_node_delete_confirm(frame, &snapshot, &delete_node_id),
        })?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match page {
                    Page::Dashboard => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Char('c') => {
                            form = FormState::from_snapshot(&snapshot);
                            page = Page::Create;
                        }
                        KeyCode::Char('n') => {
                            node_form = NodeFormState::default();
                            page = Page::NodeCreate;
                        }
                        KeyCode::Char('N') => {
                            selected_node = 0;
                            page = Page::Nodes;
                        }
                        KeyCode::Char('r') => {
                            revoke_returns_to_nodes = false;
                            revoke_node_id = snapshot
                                .nodes
                                .first()
                                .map(|node| node.id.clone())
                                .unwrap_or_default();
                            page = Page::Revoke;
                        }
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
                        KeyCode::Esc => page = Page::Dashboard,
                        KeyCode::Tab | KeyCode::Down => form.active = (form.active + 1) % 8,
                        KeyCode::BackTab | KeyCode::Up => form.active = (form.active + 7) % 8,
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::CreateSubscription(form.input()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Dashboard;
                        }
                        KeyCode::Backspace => {
                            form.fields[form.active].pop();
                        }
                        KeyCode::Char(value) => form.fields[form.active].push(value),
                        _ => {}
                    },
                    Page::NodeCreate => match key.code {
                        KeyCode::Esc => page = Page::Dashboard,
                        KeyCode::Tab | KeyCode::Down => {
                            node_form.active = (node_form.active + 1) % 10
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            node_form.active = (node_form.active + 9) % 10
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::CreateNode(node_form.input()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Dashboard;
                        }
                        KeyCode::Backspace => {
                            node_form.fields[node_form.active].pop();
                        }
                        KeyCode::Char(value) => node_form.fields[node_form.active].push(value),
                        _ => {}
                    },
                    Page::Revoke => match key.code {
                        KeyCode::Esc => {
                            page = if revoke_returns_to_nodes {
                                Page::Nodes
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
                                Page::Nodes
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
                        KeyCode::Esc | KeyCode::Left => page = Page::Dashboard,
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
                        KeyCode::Char('R') => {
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
                        KeyCode::Char('R') => {
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
                        KeyCode::Esc | KeyCode::Left => page = Page::Dashboard,
                        KeyCode::Up => selected_node = selected_node.saturating_sub(1),
                        KeyCode::Down if !snapshot.nodes.is_empty() => {
                            selected_node = (selected_node + 1).min(snapshot.nodes.len() - 1);
                        }
                        KeyCode::Char('n') => {
                            node_form = NodeFormState::default();
                            page = Page::NodeCreate;
                        }
                        KeyCode::Char('e') | KeyCode::Enter => {
                            if let Some(node) = snapshot.nodes.get(selected_node) {
                                edit_node_id = node.id.clone();
                                node_form = NodeFormState::from_node(node);
                                page = Page::NodeEdit;
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
                                    .blocking_send(TuiCommand::SetNodeStatus {
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
                                delete_node_id = node.id.clone();
                                page = Page::NodeDeleteConfirm;
                            }
                        }
                        _ => {}
                    },
                    Page::NodeEdit => match key.code {
                        KeyCode::Esc => page = Page::Nodes,
                        KeyCode::Tab | KeyCode::Down => {
                            node_form.active = (node_form.active + 1) % 10;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            node_form.active = (node_form.active + 9) % 10;
                        }
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::UpdateNode {
                                    node_id: edit_node_id.clone(),
                                    input: node_form.input(),
                                })
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Nodes;
                        }
                        KeyCode::Backspace => {
                            node_form.fields[node_form.active].pop();
                        }
                        KeyCode::Char(value) => node_form.fields[node_form.active].push(value),
                        _ => {}
                    },
                    Page::NodeDeleteConfirm => match key.code {
                        KeyCode::Esc => page = Page::Nodes,
                        KeyCode::Enter => {
                            if command_tx
                                .blocking_send(TuiCommand::DeleteNode(delete_node_id.clone()))
                                .is_err()
                            {
                                break Ok(());
                            }
                            page = Page::Nodes;
                        }
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

fn draw_dashboard(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, selected_user: usize) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new("Xenon | Dashboard")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Panel")),
        areas[0],
    );
    let mut body = format!(
        "Agents online: {}\nLast event: {}\n\nUsers (charged Xray bytes, descending)\n",
        snapshot.connected_agents,
        snapshot.last_agent_event.as_deref().unwrap_or("-")
    );
    if snapshot.users.is_empty() {
        body.push_str("  - no users\n");
    } else {
        for (index, user) in snapshot.users.iter().take(12).enumerate() {
            body.push_str(&format!(
                "{} {:<20} {:>12}  subscriptions={}  status={}\n",
                if index == selected_user { ">" } else { " " },
                truncate(&user.username, 20),
                format_bytes(user.charged_bytes),
                user.subscription_count,
                user.status
            ));
        }
    }
    body.push_str("\nNodes\n");
    if snapshot.nodes.is_empty() {
        body.push_str("  - no nodes\n");
    } else {
        for node in snapshot.nodes.iter().take(12) {
            let cpu = node
                .cpu_usage_basis_points
                .map(|value| format!("{:.1}%", value as f64 / 100.0))
                .unwrap_or_else(|| "-".into());
            let memory = match (node.memory_used_bytes, node.memory_total_bytes) {
                (Some(used), Some(total)) if total > 0 => {
                    format!("{}/{}", format_bytes(used), format_bytes(total))
                }
                _ => "-".into(),
            };
            let disk = match (node.disk_used_bytes, node.disk_total_bytes) {
                (Some(used), Some(total)) if total > 0 => {
                    format!("{}/{}", format_bytes(used), format_bytes(total))
                }
                _ => "-".into(),
            };
            let load = node
                .load_1_milli
                .map(|value| format!("{:.2}", value as f64 / 1000.0))
                .unwrap_or_else(|| "-".into());
            body.push_str(&format!(
                "  {:<18} {}:{}  {}  cpu={} mem={} disk={} load={} status={}/{} agent={}/{} xray={}\n",
                truncate(&node.name, 18),
                node.landing_host,
                node.xray_listen_port,
                node.security,
                cpu,
                memory,
                disk,
                load,
                node.management_status,
                node.node_status,
                node.agent_status.as_deref().unwrap_or("unregistered"),
                node.agent_version.as_deref().unwrap_or("-"),
                node.xray_version.as_deref().unwrap_or("-")
            ));
        }
    }
    if !snapshot.notice.is_empty() {
        body.push_str(&format!("\n{}\n", snapshot.notice));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Overview")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(
            "Up/Down User   Enter Details   c Create   n New node   N Nodes   r Revoke   q Quit",
        )
        .style(Style::default().fg(Color::Green))
        .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_nodes(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, selected_node: usize) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new("Node management")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Nodes")),
        areas[0],
    );
    let mut body = String::new();
    for (index, node) in snapshot.nodes.iter().enumerate() {
        body.push_str(&format!(
            "{} {:<18} {:<9} runtime={:<8} agent={:<12} {}:{}\n",
            if index == selected_node { ">" } else { " " },
            truncate(&node.name, 18),
            node.management_status,
            node.node_status,
            node.agent_status.as_deref().unwrap_or("unregistered"),
            node.publish_host.as_deref().unwrap_or(&node.landing_host),
            node.publish_port.unwrap_or(node.xray_listen_port),
        ));
        body.push_str(&format!(
            "    id={} security={} revision={} agent-version={} xray={}\n",
            node.id,
            node.security,
            node.desired_revision,
            node.agent_version.as_deref().unwrap_or("-"),
            node.xray_version.as_deref().unwrap_or("-"),
        ));
    }
    if snapshot.nodes.is_empty() {
        body.push_str("No nodes\n");
    }
    if !snapshot.notice.is_empty() {
        body.push_str(&format!("\n{}\n", snapshot.notice));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Inventory")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("Up/Down Select   Enter/e Edit   d Enable/disable   u Upgrade   r Revoke   D Delete   n New   Esc Back")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_node_edit(frame: &mut ratatui::Frame<'_>, form: &NodeFormState, node_id: &str) {
    draw_node_form(
        frame,
        form,
        &format!("Edit node {}", truncate(node_id, 16)),
        "Save",
    );
}

fn draw_node_delete_confirm(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, node_id: &str) {
    let text = format!(
        "Logically delete node {node_id}?\n\nActive subscription and NIC references must be removed first.\nAgent certificates will be revoked.\n\n{}\n\nEnter Confirm   Esc Cancel",
        snapshot.notice
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title("Delete node")),
        frame.area(),
    );
}

fn draw_revoke(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, node_id: &str) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new("Emergency Agent certificate revocation")
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Revoke")),
        areas[0],
    );
    let mut body = format!("Node ID: {node_id}\n\nRegistered nodes:\n");
    for node in snapshot.nodes.iter().take(12) {
        body.push_str(&format!("  {} ({})\n", node.id, node.name));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Target")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("Enter Revoke all Agent certificates   Esc Cancel")
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title("Confirm")),
        areas[2],
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
        Paragraph::new("Up/Down Select   e Edit   b NIC   R Reset   T Token   U UUID   Esc Back")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
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
        Paragraph::new("Up/Down Select   a Add   R Reset cycle   D Unbind   Esc Back")
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
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new("Add NIC binding")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("NIC billing")),
        areas[0],
    );
    let labels = [
        "Node ID",
        "Interface",
        "Traffic limit bytes",
        "Initial used bytes",
        "Direction (rx_tx, tx_only, rx_only)",
        "Reset policy",
    ];
    let mut body = String::new();
    for (index, (label, value)) in labels.iter().zip(form.fields.iter()).enumerate() {
        body.push_str(&format!(
            "{} {label}: {value}\n",
            if index == form.active { ">" } else { " " }
        ));
    }
    body.push_str("\nReported interfaces:\n");
    for interface in snapshot.interfaces.iter().take(10) {
        body.push_str(&format!(
            "  {}/{}\n",
            interface.node_id, interface.interface_name
        ));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Fields")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("Tab/Up/Down Move   Enter Add   Esc Cancel")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_nic_unbind_confirm(
    frame: &mut ratatui::Frame<'_>,
    snapshot: &TuiSnapshot,
    binding_id: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Confirm unbind");
    let text = format!(
        "Disable NIC binding {binding_id}?\n\nHistorical records remain stored.\n\n{}\n\nEnter Confirm   Esc Cancel",
        snapshot.notice
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(Color::Red))
            .block(block),
        frame.area(),
    );
}

fn draw_subscription_edit(frame: &mut ratatui::Frame<'_>, form: &SubscriptionEditFormState) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "Edit subscription {}",
            truncate(&form.subscription_id, 16)
        ))
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("Subscription")),
        areas[0],
    );
    let labels = [
        "Name",
        "Node IDs (comma separated)",
        "Xray limit bytes (empty = unlimited)",
        "Expiry Unix time (empty = never)",
        "Multiplier (1 or 2)",
        "Reset (never, manual, daily:HH:MM, monthly:DAY@HH:MM, interval:DAYS)",
        "Status (active or disabled)",
    ];
    let mut body = String::new();
    for (index, (label, value)) in labels.iter().zip(form.fields.iter()).enumerate() {
        body.push_str(&format!(
            "{} {label}: {value}\n",
            if index == form.active { ">" } else { " " }
        ));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Fields")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("Tab/Up/Down Move   Enter Save   Esc Cancel")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
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
        RotateKind::Token => (
            "subscription token",
            "The old subscription URL stops working immediately.",
        ),
        RotateKind::Uuid => (
            "Xray UUID",
            "Agents will replace this user and clients must refresh the subscription.",
        ),
    };
    let text = format!(
        "Rotate {label} for {}?\n\n{impact}\nThe new secret is displayed only once.\n\nEnter Confirm   Esc Cancel",
        subscription.map_or("-", |record| record.name.as_str())
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(Color::Red))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm rotation"),
            ),
        frame.area(),
    );
}

fn draw_create(frame: &mut ratatui::Frame<'_>, snapshot: &TuiSnapshot, form: &FormState) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new("Create user + subscription")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Wizard")),
        areas[0],
    );
    let labels = [
        "Username",
        "Subscription name",
        "Node IDs (comma separated)",
        "Xray limit bytes (empty = unlimited)",
        "Expiry Unix time (empty = never)",
        "Multiplier (1 or 2)",
        "Reset (never, manual, daily:HH:MM, monthly:DAY@HH:MM, interval:DAYS)",
        "NIC node/interface/limit/initial[/direction[/reset]] (; separated)",
    ];
    let mut body = String::new();
    for (index, (label, value)) in labels.iter().zip(form.fields.iter()).enumerate() {
        let marker = if index == form.active { ">" } else { " " };
        body.push_str(&format!("{marker} {label}: {value}\n"));
    }
    body.push_str("\nAvailable nodes:\n");
    for node in snapshot
        .nodes
        .iter()
        .filter(|node| node.management_status == "active")
        .take(8)
    {
        body.push_str(&format!("  {} ({})\n", node.id, node.name));
    }
    body.push_str("\nReported interfaces:\n");
    for interface in snapshot.interfaces.iter().take(8) {
        body.push_str(&format!(
            "  {}/{}\n",
            interface.node_id, interface.interface_name
        ));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Fields")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("Tab/Up/Down Move   Enter Create   Esc Cancel")
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
}

fn draw_node_create(frame: &mut ratatui::Frame<'_>, form: &NodeFormState) {
    draw_node_form(
        frame,
        form,
        "Create node + one-time registration token",
        "Create",
    );
}

fn draw_node_form(frame: &mut ratatui::Frame<'_>, form: &NodeFormState, title: &str, action: &str) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Node Wizard")),
        areas[0],
    );
    let labels = [
        "Node name",
        "Landing host or IP",
        "Xray listen port",
        "Optional relay host",
        "Optional relay port",
        "Security (none, tls, reality)",
        "TLS/Reality server name",
        "Reality public key",
        "Reality short ID",
        "Reality fingerprint",
    ];
    let mut body = String::new();
    for (index, (label, value)) in labels.iter().zip(form.fields.iter()).enumerate() {
        let marker = if index == form.active { ">" } else { " " };
        body.push_str(&format!("{marker} {label}: {value}\n"));
    }
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("Fields")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(format!("Tab/Up/Down Move   Enter {action}   Esc Cancel"))
            .style(Style::default().fg(Color::Green))
            .block(Block::default().borders(Borders::ALL).title("Keys")),
        areas[2],
    );
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
        agent_upgrade_command, create_node, create_subscription, format_bytes, format_reset_policy,
        CreateNodeInput, CreateSubscriptionInput, FormState, NicBindingFormState, NodeFormState,
        SubscriptionEditFormState, TuiSnapshot,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use xenon_storage::Database;

    #[test]
    fn renders_every_page_in_a_small_terminal_without_panicking() {
        let snapshot = TuiSnapshot::default();
        let form = FormState {
            fields: Default::default(),
            active: 0,
        };
        let node_form = NodeFormState::default();
        let nic_form = NicBindingFormState::from_snapshot(&snapshot, None);
        let subscription_form = SubscriptionEditFormState {
            fields: Default::default(),
            subscription_id: "subscription".into(),
            starts_at: 0,
            current_cycle_start: 0,
            active: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(24, 4)).expect("test terminal");
        terminal
            .draw(|frame| {
                super::draw_dashboard(frame, &snapshot, 0);
                super::draw_nodes(frame, &snapshot, 0);
                super::draw_node_edit(frame, &node_form, "node");
                super::draw_node_delete_confirm(frame, &snapshot, "node");
                super::draw_revoke(frame, &snapshot, "node");
                super::draw_user_detail(frame, &snapshot, 0);
                super::draw_nic_bindings(frame, &snapshot, 0, 0);
                super::draw_nic_create(frame, &snapshot, &nic_form);
                super::draw_nic_unbind_confirm(frame, &snapshot, "binding");
                super::draw_subscription_edit(frame, &subscription_form);
                super::draw_subscription_rotate_confirm(
                    frame,
                    &snapshot,
                    0,
                    super::RotateKind::Token,
                );
                super::draw_create(frame, &snapshot, &form);
                super::draw_node_create(frame, &node_form);
            })
            .expect("small terminal render");
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
            "http://127.0.0.1:18081",
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

        assert!(notice.contains("http://127.0.0.1:18081/sub/"));
        let users = database.list_user_summaries().await.expect("users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "alice");
        let subscriptions = database
            .list_user_subscriptions(&users[0].id)
            .await
            .expect("subscriptions");
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

    #[tokio::test]
    async fn creates_node_and_registration_token_atomically() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        let install = crate::config::AgentInstallConfig {
            enabled: true,
            script_url: "https://downloads.test/install-agent.sh".into(),
            binary_url: "https://downloads.test/xenon-agent".into(),
            binary_sha256: "a".repeat(64),
            binary_version: "0.1.0".into(),
            ca_url: "https://downloads.test/panel-ca.crt".into(),
            panel_endpoint: "https://panel.test:50051".into(),
            enrollment_endpoint: "https://panel.test:50052".into(),
            server_name: "panel.test".into(),
        };
        let notice = create_node(
            &database,
            "127.0.0.1:50051",
            &install,
            CreateNodeInput {
                name: "Node A".into(),
                landing_host: "203.0.113.10".into(),
                xray_port: "443".into(),
                publish_host: "relay.example".into(),
                publish_port: "8443".into(),
                security: "none".into(),
                server_name: String::new(),
                reality_public_key: String::new(),
                reality_short_id: String::new(),
                reality_fingerprint: String::new(),
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
        let token_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM registration_tokens")
            .fetch_one(database.pool())
            .await
            .expect("tokens");
        assert_eq!(token_count, 1);
    }
}
