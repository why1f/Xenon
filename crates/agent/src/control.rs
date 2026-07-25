use crate::spool::TrafficSpool;
use crate::xray_api::{TrafficTracker, XrayApi};
use crate::{collector, config::AgentConfig, xray_config::build_bootstrap};
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, ClientTlsConfig, Identity};
use tonic::Request;
use xenon_domain::{MAX_XRAY_CORE_VERSION, PANEL_AGENT_PROTOCOL_VERSION};
use xenon_protocol::panel_agent::{
    agent_control_client::AgentControlClient, agent_to_panel::Payload, panel_to_agent,
    AgentToPanel, DesiredStateAck, PanelToAgent, RegisterRequest, XrayTrafficBatch,
    XrayUserTraffic,
};
use xray_embedded_runner::{XrayStatus, XraySupervisor, EMBEDDED_SHA256, EMBEDDED_VERSION};

pub async fn run(mut config: AgentConfig, config_path: &Path) -> anyhow::Result<()> {
    ensure_certificate(&mut config, config_path).await?;
    let mut spool = TrafficSpool::open(
        &config.spool.path,
        config.spool.max_batches,
        config.spool.max_bytes,
    )
    .await?;
    let endpoint = tonic::transport::Channel::from_shared(config.panel_endpoint.clone())?;
    let channel = if config.tls.enabled {
        let ca = Certificate::from_pem(tokio::fs::read(&config.tls.ca_path).await?);
        let identity = Identity::from_pem(
            tokio::fs::read(&config.tls.cert_path).await?,
            tokio::fs::read(&config.tls.key_path).await?,
        );
        endpoint
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(ca)
                    .identity(identity)
                    .domain_name(config.tls.domain_name.clone()),
            )?
            .connect()
            .await?
    } else {
        endpoint.connect().await?
    };
    let mut client = AgentControlClient::new(channel);
    let (tx, rx) = mpsc::channel::<AgentToPanel>(32);
    tx.send(AgentToPanel {
        payload: Some(Payload::Register(RegisterRequest {
            agent_id: config.agent_id.clone(),
            node_id: config.node_id.clone(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
            xray_version: if XraySupervisor::embedded_available() {
                format!("{} ({})", EMBEDDED_VERSION, EMBEDDED_SHA256)
            } else {
                "not-configured".into()
            },
            registration_token: config.registration_token.clone(),
            max_supported_xray_version: MAX_XRAY_CORE_VERSION.into(),
            protocol_version: PANEL_AGENT_PROTOCOL_VERSION.into(),
        })),
    })
    .await?;
    let response = client
        .open_stream(Request::new(ReceiverStream::new(rx)))
        .await?;
    let mut inbound = response.into_inner();
    let registration = tokio::time::timeout(Duration::from_secs(10), inbound.message())
        .await??
        .ok_or_else(|| anyhow::anyhow!("panel closed before registration response"))?;
    match registration.payload {
        Some(panel_to_agent::Payload::Register(value))
            if value.accepted && value.panel_protocol_version == PANEL_AGENT_PROTOCOL_VERSION => {}
        Some(panel_to_agent::Payload::Register(value)) if value.accepted => {
            anyhow::bail!(
                "Panel protocol {} is incompatible with Agent protocol {}",
                value.panel_protocol_version,
                PANEL_AGENT_PROTOCOL_VERSION
            )
        }
        Some(panel_to_agent::Payload::Register(value)) => {
            anyhow::bail!("panel rejected Agent registration: {}", value.message)
        }
        _ => anyhow::bail!("panel did not send registration response"),
    }
    config.clear_registration_token(config_path).await?;
    for batch in spool.pending_batches() {
        if batch.agent_id != config.agent_id || batch.node_id != config.node_id {
            anyhow::bail!("traffic spool identity does not match Agent configuration");
        }
        tx.send(AgentToPanel {
            payload: Some(Payload::XrayTraffic(batch)),
        })
        .await?;
    }
    let (command_tx, mut command_rx) = mpsc::channel::<PanelToAgent>(16);
    let _receive_task = tokio::spawn(async move {
        while let Some(message) = inbound.message().await? {
            command_tx
                .send(message)
                .await
                .map_err(|_| tonic::Status::cancelled("agent control loop stopped"))?;
        }
        Ok::<(), tonic::Status>(())
    });
    let start = tokio::time::Instant::now();
    let mut supervisor = XraySupervisor::default();
    let xray_bootstrap = match config.xray.bootstrap_json.as_deref() {
        Some(value) => Some(value.as_bytes().to_vec()),
        None => Some(build_bootstrap(&config.xray)?),
    };
    let mut restart_at = None;
    let mut restart_failures = 0_u32;
    if let Some(config_json) = xray_bootstrap.as_deref() {
        if XraySupervisor::embedded_available() {
            if let Err(error) = supervisor.start(config_json).await {
                tracing::error!(%error, "initial Xray start failed");
                restart_at = Some(tokio::time::Instant::now() + Duration::from_secs(1));
            }
        } else {
            tracing::warn!(
                "Xray bootstrap_json is configured but no embedded Xray asset is available"
            );
        }
    }
    let mut xray_api = XrayApi::new(&config.xray.api_endpoint, config.xray.inbound_tag.clone())?;
    let mut desired_users = HashMap::<String, (String, String)>::new();
    let mut applied_users = HashMap::<String, String>::new();
    let mut traffic_tracker = TrafficTracker::default();
    let mut xray_instance_id = uuid::Uuid::now_v7().to_string();
    let mut desired_revision = 0;
    let mut applied_revision = 0;
    let mut reconcile_pending = false;
    let mut traffic_sequence = 0;
    let mut last_traffic_sample = collector::now_unix();
    let mut system_collector = collector::SystemCollector::default();
    let mut ticker = tokio::time::interval(Duration::from_secs(config.interval_seconds.max(1)));
    let mut watchdog = tokio::time::interval(Duration::from_millis(250));
    let mut interface_sequence = 0;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                interface_sequence += 1;
                let mut heartbeat = collector::heartbeat(&config.agent_id, &config.node_id, start.elapsed().as_secs());
                if let Some(Payload::Heartbeat(value)) = heartbeat.payload.as_mut() {
                    value.desired_revision = desired_revision;
                    value.applied_revision = applied_revision;
                    value.xray_running = supervisor.status() == XrayStatus::Running;
                    value.xray_restart_count = supervisor.restart_count();
                }
                tx.send(heartbeat).await?;
                tx.send(collector::interface_snapshot(&config.agent_id, &config.node_id, interface_sequence)).await?;
                tx.send(system_collector.snapshot(&config.agent_id, &config.node_id, interface_sequence)).await?;

                if supervisor.status() == XrayStatus::Running {
                    if reconcile_pending {
                        let (_, failures, error) = reconcile_user_map(
                            &mut xray_api,
                            &mut applied_users,
                            &desired_users,
                        ).await;
                        reconcile_pending = failures > 0;
                        if failures == 0 {
                            applied_revision = desired_revision;
                        } else {
                            tracing::warn!(%error, failures, "Xray user reconciliation retry failed");
                        }
                    }
                    let interval_end = collector::now_unix();
                    let interval_start = last_traffic_sample;
                    last_traffic_sample = interval_end;
                    match xray_api.query_user_traffic().await {
                        Ok(current) => {
                            let deltas = traffic_tracker.observe(current);
                            let users = deltas.into_iter().filter_map(|traffic| {
                                desired_users.get(&traffic.email).map(|(subscription_id, _)| XrayUserTraffic {
                                    subscription_id: subscription_id.clone(),
                                    xray_email: traffic.email,
                                    uplink_delta: traffic.uplink,
                                    downlink_delta: traffic.downlink,
                                })
                            }).collect::<Vec<_>>();
                            if !users.is_empty() {
                                traffic_sequence += 1;
                                let batch = XrayTrafficBatch {
                                    agent_id: config.agent_id.clone(),
                                    node_id: config.node_id.clone(),
                                    xray_instance_id: xray_instance_id.clone(),
                                    sequence: traffic_sequence,
                                    interval_start_unix: interval_start,
                                    interval_end_unix: interval_end,
                                    users,
                                };
                                spool.enqueue(&batch).await?;
                                tx.send(AgentToPanel { payload: Some(Payload::XrayTraffic(batch))}).await?;
                            }
                        }
                        Err(error) => tracing::debug!(%error, "Xray stats query failed"),
                    }
                }
            }
            _ = watchdog.tick() => {
                if let Some(exit) = supervisor.poll()? {
                    tracing::warn!(?exit, "embedded Xray exited");
                    traffic_tracker.reset();
                    applied_users.clear();
                    reconcile_pending = true;
                    restart_at = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                }
                let restart_due = restart_at.is_some_and(|deadline| deadline <= tokio::time::Instant::now());
                if restart_due {
                    let Some(config_json) = xray_bootstrap.as_deref() else {
                        restart_at = None;
                        continue;
                    };
                    match supervisor.start(config_json).await {
                        Ok(()) => {
                            restart_at = None;
                            restart_failures = 0;
                            xray_instance_id = uuid::Uuid::now_v7().to_string();
                            traffic_sequence = 0;
                            last_traffic_sample = collector::now_unix();
                            traffic_tracker.reset();
                            let (_, failures, error) = reconcile_user_map(
                                &mut xray_api,
                                &mut applied_users,
                                &desired_users,
                            ).await;
                            if failures == 0 {
                                applied_revision = desired_revision;
                                reconcile_pending = false;
                            } else {
                                reconcile_pending = true;
                                tracing::warn!(%error, failures, "Xray users failed to reconcile after restart");
                            }
                        }
                        Err(error) => {
                            let delay = (1_u64 << restart_failures.min(6)).min(60);
                            restart_failures = restart_failures.saturating_add(1);
                            restart_at = Some(tokio::time::Instant::now() + Duration::from_secs(delay));
                            tracing::error!(%error, delay, "Xray restart failed");
                        }
                    }
                }
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    supervisor.stop().await?;
                    anyhow::bail!("panel stream closed");
                };
                match command.payload {
                    Some(panel_to_agent::Payload::TrafficAck(ack)) => {
                        spool.acknowledge(&ack.batch_id).await?;
                    }
                    Some(panel_to_agent::Payload::RestartXray(_)) => {
                        supervisor.stop().await?;
                        applied_users.clear();
                        traffic_tracker.reset();
                        reconcile_pending = true;
                        restart_at = Some(tokio::time::Instant::now());
                    }
                    Some(panel_to_agent::Payload::Register(_)) | None => {}
                    Some(panel_to_agent::Payload::DesiredState(snapshot)) => {
                        let (success_count, failure_count, error_message) = apply_desired_state(&mut xray_api, &mut applied_users, &mut desired_users, &snapshot).await;
                        desired_revision = snapshot.revision;
                        if failure_count == 0 {
                            applied_revision = snapshot.revision;
                        }
                        reconcile_pending = failure_count > 0;
                        tx.send(AgentToPanel { payload: Some(Payload::DesiredStateAck(DesiredStateAck {
                            agent_id: config.agent_id.clone(),
                            node_id: config.node_id.clone(),
                            revision: snapshot.revision,
                            success_count,
                            failure_count,
                            error_message,
                        }))}).await?;
                    }
                }
            }
        }
    }
}

async fn ensure_certificate(config: &mut AgentConfig, config_path: &Path) -> anyhow::Result<()> {
    if !config.tls.enabled {
        return Ok(());
    }
    let cert_exists = tokio::fs::try_exists(&config.tls.cert_path).await?;
    let key_exists = tokio::fs::try_exists(&config.tls.key_path).await?;
    if cert_exists && key_exists {
        let renew_before = i64::from(config.tls.renew_before_days) * 86_400;
        if config
            .tls
            .certificate_expires_at
            .is_some_and(|expires| expires <= collector::now_unix() as i64 + renew_before)
        {
            rotate_certificate(config, config_path).await?;
        }
        return Ok(());
    }
    if cert_exists != key_exists {
        anyhow::bail!(
            "Agent certificate/key pair is incomplete; restore both files or remove both"
        );
    }
    if config.registration_token.trim().is_empty() {
        anyhow::bail!("Agent certificate/key are missing and registration_token is empty");
    }
    let cert_path = Path::new(&config.tls.cert_path);
    let key_path = Path::new(&config.tls.key_path);
    if let Some(parent) = cert_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = key_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let key_pair = if tokio::fs::try_exists(key_path).await? {
        KeyPair::from_pem(&tokio::fs::read_to_string(key_path).await?)?
    } else {
        let key_pair = KeyPair::generate()?;
        tokio::fs::write(key_path, key_pair.serialize_pem()).await?;
        set_private_file_permissions(key_path).await?;
        key_pair
    };
    let ca_pem = tokio::fs::read(&config.tls.ca_path).await?;
    let endpoint = tonic::transport::Channel::from_shared(config.tls.enrollment_endpoint.clone())?;
    let channel = endpoint
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(ca_pem.clone()))
                .domain_name(config.tls.domain_name.clone()),
        )?
        .connect()
        .await?;
    let mut client =
        xenon_protocol::panel_agent::agent_enrollment_client::AgentEnrollmentClient::new(channel);
    let csr = certificate_signing_request(&key_pair, &config.agent_id)?;
    let response = client
        .enroll(tonic::Request::new(
            xenon_protocol::panel_agent::EnrollRequest {
                agent_id: config.agent_id.clone(),
                node_id: config.node_id.clone(),
                agent_version: env!("CARGO_PKG_VERSION").into(),
                xray_version: if XraySupervisor::embedded_available() {
                    format!("{} ({})", EMBEDDED_VERSION, EMBEDDED_SHA256)
                } else {
                    "not-configured".into()
                },
                max_supported_xray_version: MAX_XRAY_CORE_VERSION.into(),
                protocol_version: PANEL_AGENT_PROTOCOL_VERSION.into(),
                registration_token: config.registration_token.clone(),
                csr_pem: csr,
            },
        ))
        .await?
        .into_inner();
    if !response.accepted
        || response.certificate_pem.trim().is_empty()
        || response.client_ca_pem.trim().is_empty()
    {
        anyhow::bail!(
            "Panel rejected Agent certificate enrollment: {}",
            response.message
        );
    }
    tokio::fs::write(cert_path, response.certificate_pem).await?;
    set_private_file_permissions(cert_path).await?;
    config.tls.certificate_expires_at = Some(response.expires_at_unix);
    config.persist(config_path).await?;
    tracing::info!(agent_id = %config.agent_id, expires_at = response.expires_at_unix, "Agent certificate enrolled");
    Ok(())
}

async fn rotate_certificate(config: &mut AgentConfig, config_path: &Path) -> anyhow::Result<()> {
    let next_key_path = format!("{}.next", config.tls.key_path);
    let next_cert_path = format!("{}.next", config.tls.cert_path);
    let next_key_path_ref = Path::new(&next_key_path);
    if let Some(parent) = next_key_path_ref.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let next_key = if tokio::fs::try_exists(next_key_path_ref).await? {
        KeyPair::from_pem(&tokio::fs::read_to_string(next_key_path_ref).await?)?
    } else {
        let key = KeyPair::generate()?;
        tokio::fs::write(next_key_path_ref, key.serialize_pem()).await?;
        set_private_file_permissions(next_key_path_ref).await?;
        key
    };
    let endpoint = tonic::transport::Channel::from_shared(config.panel_endpoint.clone())?;
    let channel = endpoint
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(
                    tokio::fs::read(&config.tls.ca_path).await?,
                ))
                .identity(Identity::from_pem(
                    tokio::fs::read(&config.tls.cert_path).await?,
                    tokio::fs::read(&config.tls.key_path).await?,
                ))
                .domain_name(config.tls.domain_name.clone()),
        )?
        .connect()
        .await?;
    let mut client = AgentControlClient::new(channel);
    let response = client
        .rotate_certificate(tonic::Request::new(
            xenon_protocol::panel_agent::RotateCertificateRequest {
                agent_id: config.agent_id.clone(),
                node_id: config.node_id.clone(),
                max_supported_xray_version: MAX_XRAY_CORE_VERSION.into(),
                csr_pem: certificate_signing_request(&next_key, &config.agent_id)?,
                protocol_version: PANEL_AGENT_PROTOCOL_VERSION.into(),
            },
        ))
        .await?
        .into_inner();
    if !response.accepted || response.certificate_pem.trim().is_empty() {
        anyhow::bail!(
            "Panel rejected Agent certificate rotation: {}",
            response.message
        );
    }
    tokio::fs::write(&next_cert_path, response.certificate_pem).await?;
    set_private_file_permissions(Path::new(&next_cert_path)).await?;
    config.tls.key_path = next_key_path;
    config.tls.cert_path = next_cert_path;
    config.tls.certificate_expires_at = Some(response.expires_at_unix);
    config.persist(config_path).await?;
    tracing::info!(agent_id = %config.agent_id, expires_at = response.expires_at_unix, "Agent certificate rotated");
    Ok(())
}

fn certificate_signing_request(key_pair: &KeyPair, agent_id: &str) -> anyhow::Result<String> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
        .distinguished_name
        .push(DnType::CommonName, format!("xenon-agent:{agent_id}"));
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    Ok(params.serialize_request(key_pair)?.pem()?)
}

async fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(path).await?.permissions();
        permissions.set_mode(0o600);
        tokio::fs::set_permissions(path, permissions).await?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

async fn apply_desired_state(
    api: &mut XrayApi,
    applied: &mut HashMap<String, String>,
    desired: &mut HashMap<String, (String, String)>,
    snapshot: &xenon_protocol::panel_agent::DesiredStateSnapshot,
) -> (u32, u32, String) {
    let next = snapshot
        .desired_users
        .iter()
        .filter(|user| user.enabled)
        .map(|user| {
            (
                user.xray_email.clone(),
                (user.subscription_id.clone(), user.uuid.clone()),
            )
        })
        .collect::<HashMap<_, _>>();
    let result = reconcile_user_map(api, applied, &next).await;
    *desired = next;
    result
}

async fn reconcile_user_map(
    api: &mut XrayApi,
    applied: &mut HashMap<String, String>,
    next: &HashMap<String, (String, String)>,
) -> (u32, u32, String) {
    let mut success = 0;
    let mut failures = 0;
    let mut first_error = String::new();
    for email in applied
        .keys()
        .filter(|email| !next.contains_key(*email))
        .cloned()
        .collect::<Vec<_>>()
    {
        match api.remove_user(&email).await {
            Ok(()) => {
                applied.remove(&email);
                success += 1;
            }
            Err(error) => {
                failures += 1;
                if first_error.is_empty() {
                    first_error = error.to_string();
                }
            }
        }
    }
    for (email, (_, uuid)) in next {
        if applied.get(email) == Some(uuid) {
            continue;
        }
        if applied.contains_key(email) {
            if let Err(error) = api.remove_user(email).await {
                failures += 1;
                if first_error.is_empty() {
                    first_error = error.to_string();
                }
                continue;
            }
        }
        match api.add_vless_user(email, uuid, "").await {
            Ok(()) => {
                applied.insert(email.clone(), uuid.clone());
                success += 1;
            }
            Err(error) => {
                failures += 1;
                if first_error.is_empty() {
                    first_error = error.to_string();
                }
            }
        }
    }
    (success, failures, first_error)
}
