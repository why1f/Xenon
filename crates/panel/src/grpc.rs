use crate::{
    config::{EnrollmentConfig, RegistrationConfig, TlsConfig},
    secrets::sha256_hex,
    RuntimeState,
};
use chrono::{Datelike, Duration as ChronoDuration, Utc};
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PublicKeyData, SerialNumber,
};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::transport::{Certificate, Identity, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use xenon_protocol::panel_agent::{
    agent_control_server::{AgentControl, AgentControlServer},
    agent_enrollment_server::{AgentEnrollment, AgentEnrollmentServer},
    agent_to_panel::Payload as AgentPayload,
    panel_to_agent::Payload as PanelPayload,
    AgentToPanel, DesiredStateSnapshot, EnrollRequest, EnrollResponse, PanelToAgent,
    RegisterResponse, RotateCertificateRequest, TrafficAck, XrayUser,
};
use xenon_storage::Database;

type ResponseStream = Pin<Box<dyn Stream<Item = Result<PanelToAgent, Status>> + Send + 'static>>;

pub async fn serve(
    addr: String,
    state: Arc<RwLock<RuntimeState>>,
    database: Database,
    tls: TlsConfig,
    registration: RegistrationConfig,
    enrollment: EnrollmentConfig,
) -> anyhow::Result<()> {
    let addr = addr.parse()?;
    let mut server = tonic::transport::Server::builder();
    if tls.enabled {
        let identity = Identity::from_pem(
            tokio::fs::read(&tls.cert_path).await?,
            tokio::fs::read(&tls.key_path).await?,
        );
        let client_ca = Certificate::from_pem(tokio::fs::read(&tls.client_ca_path).await?);
        server = server.tls_config(
            ServerTlsConfig::new()
                .identity(identity)
                .client_ca_root(client_ca),
        )?;
    }
    let tls_enabled = tls.enabled;
    let certificate_issuer = if enrollment.enabled {
        Some(CertificateIssuer {
            ca_certificate_pem: tokio::fs::read_to_string(&enrollment.ca_cert_path).await?,
            ca_key_pem: tokio::fs::read_to_string(&enrollment.ca_key_path).await?,
            certificate_valid_days: enrollment.certificate_valid_days,
        })
    } else {
        None
    };
    server
        .add_service(AgentControlServer::new(AgentService {
            state,
            database,
            registration,
            tls_enabled,
            certificate_issuer,
        }))
        .serve(addr)
        .await?;
    Ok(())
}

pub async fn serve_enrollment(
    tls: TlsConfig,
    enrollment: EnrollmentConfig,
    database: Database,
) -> anyhow::Result<()> {
    let identity = Identity::from_pem(
        tokio::fs::read(&tls.cert_path).await?,
        tokio::fs::read(&tls.key_path).await?,
    );
    let service = EnrollmentService {
        database,
        issuer: CertificateIssuer {
            ca_certificate_pem: tokio::fs::read_to_string(&enrollment.ca_cert_path).await?,
            ca_key_pem: tokio::fs::read_to_string(&enrollment.ca_key_path).await?,
            certificate_valid_days: enrollment.certificate_valid_days,
        },
    };
    tonic::transport::Server::builder()
        .tls_config(ServerTlsConfig::new().identity(identity))?
        .add_service(AgentEnrollmentServer::new(service))
        .serve(enrollment.addr.parse()?)
        .await?;
    Ok(())
}

struct EnrollmentService {
    database: Database,
    issuer: CertificateIssuer,
}

#[derive(Clone)]
struct CertificateIssuer {
    ca_certificate_pem: String,
    ca_key_pem: String,
    certificate_valid_days: u32,
}

#[tonic::async_trait]
impl AgentEnrollment for EnrollmentService {
    async fn enroll(
        &self,
        request: Request<EnrollRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        let request = request.into_inner();
        if request.agent_id.trim().is_empty()
            || request.node_id.trim().is_empty()
            || request.registration_token.len() < 32
            || request.csr_pem.len() > 32 * 1024
        {
            return Err(Status::invalid_argument("invalid enrollment request"));
        }
        validate_agent_compatibility(
            &request.protocol_version,
            &request.max_supported_xray_version,
            &request.xray_version,
        )
        .map_err(Status::failed_precondition)?;
        let now = Utc::now().timestamp();
        let token_hash = sha256_hex(request.registration_token.as_bytes());
        if !self
            .database
            .registration_token_can_enroll(&token_hash, &request.agent_id, &request.node_id, now)
            .await
            .map_err(internal_status)?
        {
            return Err(Status::unauthenticated(
                "invalid or expired registration token",
            ));
        }
        let issued = issue_client_certificate(
            &request.csr_pem,
            &request.agent_id,
            &self.issuer.ca_certificate_pem,
            &self.issuer.ca_key_pem,
            self.issuer.certificate_valid_days,
        )
        .map_err(|error| {
            tracing::warn!(%error, "invalid Agent certificate signing request");
            Status::invalid_argument("invalid certificate signing request")
        })?;
        let stored = self
            .database
            .enroll_agent_with_token(
                &token_hash,
                &request.agent_id,
                &request.node_id,
                &request.agent_version,
                &request.xray_version,
                &request.max_supported_xray_version,
                &issued.fingerprint,
                &issued.certificate_pem,
                &issued.public_key_sha256,
                issued.expires_at,
                now,
            )
            .await
            .map_err(internal_status)?
            .ok_or_else(|| Status::unauthenticated("enrollment token already consumed"))?;
        Ok(Response::new(EnrollResponse {
            accepted: true,
            message: "Agent certificate issued".into(),
            certificate_pem: stored.certificate_pem,
            client_ca_pem: self.issuer.ca_certificate_pem.clone(),
            expires_at_unix: stored.expires_at,
        }))
    }
}

struct IssuedCertificate {
    fingerprint: String,
    certificate_pem: String,
    public_key_sha256: String,
    expires_at: i64,
}

fn issue_client_certificate(
    csr_pem: &str,
    agent_id: &str,
    ca_certificate_pem: &str,
    ca_key_pem: &str,
    valid_days: u32,
) -> anyhow::Result<IssuedCertificate> {
    let parsed = CertificateSigningRequestParams::from_pem(csr_pem)?;
    let today = Utc::now().date_naive();
    let expires = today + ChronoDuration::days(i64::from(valid_days));
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.not_before = rcgen::date_time_ymd(
        today.year(),
        u8::try_from(today.month())?,
        u8::try_from(today.day())?,
    );
    params.not_after = rcgen::date_time_ymd(
        expires.year(),
        u8::try_from(expires.month())?,
        u8::try_from(expires.day())?,
    );
    params
        .distinguished_name
        .push(DnType::CommonName, format!("xenon-agent:{agent_id}"));
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.serial_number = Some(SerialNumber::from_slice(uuid::Uuid::now_v7().as_bytes()));
    let public_key_sha256 = sha256_hex(&parsed.public_key.subject_public_key_info());
    let request = CertificateSigningRequestParams {
        params,
        public_key: parsed.public_key,
    };
    let ca_key = KeyPair::from_pem(ca_key_pem)?;
    let issuer = Issuer::from_ca_cert_pem(ca_certificate_pem, ca_key)?;
    let certificate = request.signed_by(&issuer)?;
    Ok(IssuedCertificate {
        fingerprint: sha256_hex(certificate.der().as_ref()),
        certificate_pem: certificate.pem(),
        public_key_sha256,
        expires_at: expires
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid certificate expiry"))?
            .and_utc()
            .timestamp(),
    })
}

struct AgentService {
    state: Arc<RwLock<RuntimeState>>,
    database: Database,
    registration: RegistrationConfig,
    tls_enabled: bool,
    certificate_issuer: Option<CertificateIssuer>,
}

#[tonic::async_trait]
impl AgentControl for AgentService {
    type OpenStreamStream = ResponseStream;

    async fn open_stream(
        &self,
        request: Request<Streaming<AgentToPanel>>,
    ) -> Result<Response<Self::OpenStreamStream>, Status> {
        let certificate_fingerprint = peer_certificate_fingerprint(&request);
        let mut inbound = request.into_inner();
        let first = tokio::time::timeout(Duration::from_secs(10), inbound.next())
            .await
            .map_err(|_| Status::deadline_exceeded("registration frame timeout"))?
            .ok_or_else(|| Status::unauthenticated("registration frame required"))?
            .map_err(|error| Status::unauthenticated(error.to_string()))?;
        let register = match first.payload {
            Some(AgentPayload::Register(value)) => value,
            _ => return Err(Status::unauthenticated("first frame must be registration")),
        };
        self.authenticate(&register, certificate_fingerprint.as_deref())
            .await?;
        let enforce_certificate_binding = self.tls_enabled
            && !(self.registration.allow_insecure_dev_token
                && register.registration_token == "development-only");

        let (desired_revision, desired_users) = self
            .database
            .desired_xray_users_for_node(&register.node_id, Utc::now().timestamp())
            .await
            .map_err(internal_status)?;

        let state = self.state.clone();
        let database = self.database.clone();
        let agent_id = register.agent_id.clone();
        let node_id = register.node_id.clone();
        let stream_certificate_fingerprint = certificate_fingerprint.clone();
        let (tx, rx) = mpsc::channel(16);
        let response_tx = tx.clone();
        let mut sent_revision = desired_revision;
        let mut sent_signature = desired_signature(&desired_users);
        {
            let mut state = state.write().await;
            state.connected_agents = state.connected_agents.saturating_add(1);
            state.last_agent_event = Some(describe_registration(&register));
        }
        tokio::spawn(async move {
            while let Some(result) = inbound.next().await {
                match result {
                    Ok(message) => {
                        if enforce_certificate_binding {
                            let Some(fingerprint) = stream_certificate_fingerprint.as_deref()
                            else {
                                break;
                            };
                            match database
                                .agent_certificate_matches(
                                    &agent_id,
                                    &node_id,
                                    fingerprint,
                                    Utc::now().timestamp(),
                                )
                                .await
                            {
                                Ok(true) => {}
                                Ok(false) => {
                                    tracing::warn!(%agent_id, "Agent certificate was revoked");
                                    break;
                                }
                                Err(error) => {
                                    tracing::warn!(%error, %agent_id, "failed to verify Agent certificate state");
                                    break;
                                }
                            }
                        }
                        if !message_matches_agent(&message, &agent_id, &node_id) {
                            tracing::warn!(%agent_id, %node_id, "agent stream identity mismatch");
                            break;
                        }
                        let now = Utc::now().timestamp();
                        match &message.payload {
                            Some(AgentPayload::Interfaces(batch)) => {
                                let interfaces = batch
                                    .interfaces
                                    .iter()
                                    .map(|item| (item.name.clone(), item.rx_bytes, item.tx_bytes))
                                    .collect::<Vec<_>>();
                                if let Err(error) = database
                                    .insert_interface_snapshots(
                                        &node_id,
                                        &batch.boot_id,
                                        batch.sequence,
                                        batch.sampled_at_unix,
                                        &interfaces,
                                    )
                                    .await
                                {
                                    tracing::warn!(%error, %agent_id, "failed to persist interface snapshots");
                                }
                            }
                            Some(AgentPayload::XrayTraffic(batch)) => {
                                let mut persisted = true;
                                for user in &batch.users {
                                    let event_id = sha256_hex(
                                        format!(
                                            "{}:{}:{}:{}",
                                            agent_id,
                                            batch.xray_instance_id,
                                            batch.sequence,
                                            user.subscription_id
                                        )
                                        .as_bytes(),
                                    );
                                    if let Err(error) = database
                                        .insert_xray_traffic_event(
                                            &event_id,
                                            &agent_id,
                                            &node_id,
                                            &user.subscription_id,
                                            &batch.xray_instance_id,
                                            batch.sequence,
                                            batch.interval_start_unix,
                                            batch.interval_end_unix,
                                            user.uplink_delta,
                                            user.downlink_delta,
                                            now,
                                        )
                                        .await
                                    {
                                        persisted = false;
                                        tracing::warn!(%error, %agent_id, "failed to persist Xray traffic");
                                        break;
                                    }
                                }
                                if persisted {
                                    let batch_id = format!(
                                        "{}:{}:{}",
                                        agent_id, batch.xray_instance_id, batch.sequence
                                    );
                                    if response_tx
                                        .send(Ok(PanelToAgent {
                                            payload: Some(PanelPayload::TrafficAck(TrafficAck {
                                                batch_id,
                                                accepted_sequence: batch.sequence,
                                            })),
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                            Some(AgentPayload::System(snapshot)) => {
                                if let Err(error) = database
                                    .insert_system_snapshot(
                                        &node_id,
                                        snapshot.sequence,
                                        snapshot.sampled_at_unix,
                                        snapshot.cpu_usage_basis_points,
                                        snapshot.load_1_milli,
                                        snapshot.load_5_milli,
                                        snapshot.load_15_milli,
                                        snapshot.memory_total_bytes,
                                        snapshot.memory_used_bytes,
                                        snapshot.disk_total_bytes,
                                        snapshot.disk_used_bytes,
                                    )
                                    .await
                                {
                                    tracing::warn!(%error, %agent_id, "failed to persist system snapshot");
                                }
                            }
                            _ => {}
                        }
                        let boot_id = match &message.payload {
                            Some(AgentPayload::Interfaces(value)) => Some(value.boot_id.as_str()),
                            _ => None,
                        };
                        if let Err(error) = database.touch_agent(&agent_id, boot_id, now).await {
                            tracing::warn!(%error, %agent_id, "failed to persist agent heartbeat");
                        }
                        if let Ok((revision, users)) =
                            database.desired_xray_users_for_node(&node_id, now).await
                        {
                            let signature = desired_signature(&users);
                            if revision != sent_revision || signature != sent_signature {
                                if response_tx
                                    .send(Ok(desired_state_message(revision, users)))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                sent_revision = revision;
                                sent_signature = signature;
                            }
                        }
                        let event = describe_message(&message);
                        let mut state = state.write().await;
                        state.last_agent_event = Some(event);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "agent stream read failed");
                        break;
                    }
                }
            }
            if let Err(error) = database
                .mark_agent_offline(&agent_id, Utc::now().timestamp())
                .await
            {
                tracing::warn!(%error, %agent_id, "failed to mark agent offline");
            }
            let mut state = state.write().await;
            state.connected_agents = state.connected_agents.saturating_sub(1);
        });
        tx.send(Ok(PanelToAgent {
            payload: Some(PanelPayload::Register(RegisterResponse {
                accepted: true,
                message: "agent registered".into(),
                panel_protocol_version: xenon_domain::PANEL_AGENT_PROTOCOL_VERSION.into(),
            })),
        }))
        .await
        .map_err(|_| Status::internal("response stream closed"))?;
        tx.send(Ok(desired_state_message(desired_revision, desired_users)))
            .await
            .map_err(|_| Status::internal("response stream closed"))?;
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn rotate_certificate(
        &self,
        request: Request<RotateCertificateRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        let current_fingerprint = peer_certificate_fingerprint(&request)
            .ok_or_else(|| Status::unauthenticated("mTLS client certificate is required"))?;
        let request = request.into_inner();
        if request.agent_id.trim().is_empty()
            || request.node_id.trim().is_empty()
            || request.csr_pem.len() > 32 * 1024
            || request.max_supported_xray_version != xenon_domain::MAX_XRAY_CORE_VERSION
            || request.protocol_version != xenon_domain::PANEL_AGENT_PROTOCOL_VERSION
        {
            return Err(Status::invalid_argument(
                "invalid certificate rotation request",
            ));
        }
        let now = Utc::now().timestamp();
        if !self
            .database
            .agent_certificate_matches(
                &request.agent_id,
                &request.node_id,
                &current_fingerprint,
                now,
            )
            .await
            .map_err(internal_status)?
        {
            return Err(Status::unauthenticated("Agent certificate is not active"));
        }
        let issuer = self
            .certificate_issuer
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("certificate rotation is disabled"))?;
        let issued = issue_client_certificate(
            &request.csr_pem,
            &request.agent_id,
            &issuer.ca_certificate_pem,
            &issuer.ca_key_pem,
            issuer.certificate_valid_days,
        )
        .map_err(|error| {
            tracing::warn!(%error, "invalid certificate rotation CSR");
            Status::invalid_argument("invalid certificate signing request")
        })?;
        let stored = self
            .database
            .rotate_agent_certificate(
                &request.agent_id,
                &request.node_id,
                &current_fingerprint,
                &issued.fingerprint,
                &issued.certificate_pem,
                &issued.public_key_sha256,
                issued.expires_at,
                now,
            )
            .await
            .map_err(internal_status)?
            .ok_or_else(|| Status::unauthenticated("certificate rotation rejected"))?;
        Ok(Response::new(EnrollResponse {
            accepted: true,
            message: "Agent certificate rotated".into(),
            certificate_pem: stored.certificate_pem,
            client_ca_pem: issuer.ca_certificate_pem.clone(),
            expires_at_unix: stored.expires_at,
        }))
    }
}

impl AgentService {
    async fn authenticate(
        &self,
        register: &xenon_protocol::panel_agent::RegisterRequest,
        certificate_fingerprint: Option<&str>,
    ) -> Result<(), Status> {
        if register.agent_id.trim().is_empty() || register.node_id.trim().is_empty() {
            return Err(Status::invalid_argument(
                "agent_id and node_id are required",
            ));
        }
        validate_agent_compatibility(
            &register.protocol_version,
            &register.max_supported_xray_version,
            &register.xray_version,
        )
        .map_err(Status::failed_precondition)?;
        let now = Utc::now().timestamp();
        if self.registration.allow_insecure_dev_token
            && register.registration_token == "development-only"
        {
            self.database
                .ensure_development_node(&register.node_id, now)
                .await
                .map_err(internal_status)?;
            self.persist_agent(register, now).await?;
            return Ok(());
        }
        if self.tls_enabled && certificate_fingerprint.is_none() {
            return Err(Status::unauthenticated(
                "mTLS client certificate is required",
            ));
        }
        if let Some(fingerprint) = certificate_fingerprint {
            if self.tls_enabled
                && self
                    .database
                    .agent_certificate_matches(
                        &register.agent_id,
                        &register.node_id,
                        fingerprint,
                        now,
                    )
                    .await
                    .map_err(internal_status)?
            {
                self.database
                    .activate_agent_certificate(
                        &register.agent_id,
                        &register.node_id,
                        fingerprint,
                        now,
                    )
                    .await
                    .map_err(internal_status)?;
                self.persist_agent(register, now).await?;
                return Ok(());
            }
        }
        let token_hash = sha256_hex(register.registration_token.as_bytes());
        let accepted = self
            .database
            .register_agent_with_token(
                &token_hash,
                &register.agent_id,
                &register.node_id,
                &register.agent_version,
                &register.xray_version,
                &register.max_supported_xray_version,
                certificate_fingerprint,
                now,
            )
            .await
            .map_err(internal_status)?;
        if !accepted {
            return Err(Status::unauthenticated(
                "invalid or expired registration token",
            ));
        }
        Ok(())
    }

    async fn persist_agent(
        &self,
        register: &xenon_protocol::panel_agent::RegisterRequest,
        now: i64,
    ) -> Result<(), Status> {
        self.database
            .upsert_agent(
                &register.agent_id,
                &register.node_id,
                &register.agent_version,
                &register.xray_version,
                &register.max_supported_xray_version,
                now,
            )
            .await
            .map_err(internal_status)
    }
}

fn peer_certificate_fingerprint<T>(request: &Request<T>) -> Option<String> {
    request
        .peer_certs()?
        .first()
        .map(|certificate| sha256_hex(certificate.as_ref()))
}

fn internal_status(error: impl std::fmt::Display) -> Status {
    tracing::error!(%error, "agent registration storage failure");
    Status::internal("registration storage failure")
}

fn validate_agent_compatibility(
    protocol_version: &str,
    max_supported_xray_version: &str,
    xray_version: &str,
) -> Result<(), &'static str> {
    if protocol_version != xenon_domain::PANEL_AGENT_PROTOCOL_VERSION {
        return Err("unsupported Panel/Agent protocol version");
    }
    if max_supported_xray_version != xenon_domain::MAX_XRAY_CORE_VERSION {
        return Err("unsupported Xray version policy");
    }
    if xray_version != "not-configured" {
        let reported = xray_version
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_start_matches('v');
        if reported != xenon_domain::MAX_XRAY_CORE_VERSION {
            return Err("Agent embeds an unsupported Xray version");
        }
    }
    Ok(())
}

fn describe_registration(value: &xenon_protocol::panel_agent::RegisterRequest) -> String {
    format!(
        "registered agent {} (xray={}, max={})",
        value.agent_id, value.xray_version, value.max_supported_xray_version
    )
}

fn message_matches_agent(message: &AgentToPanel, agent_id: &str, node_id: &str) -> bool {
    match &message.payload {
        Some(AgentPayload::Heartbeat(value)) => {
            value.agent_id == agent_id && value.node_id == node_id
        }
        Some(AgentPayload::Interfaces(value)) => {
            value.agent_id == agent_id && value.node_id == node_id
        }
        Some(AgentPayload::XrayTraffic(value)) => {
            value.agent_id == agent_id && value.node_id == node_id
        }
        Some(AgentPayload::DesiredStateAck(value)) => {
            value.agent_id == agent_id && value.node_id == node_id
        }
        Some(AgentPayload::System(value)) => value.agent_id == agent_id && value.node_id == node_id,
        Some(AgentPayload::Register(_)) | None => false,
    }
}

fn describe_message(message: &AgentToPanel) -> String {
    match &message.payload {
        Some(AgentPayload::Register(value)) => describe_registration(value),
        Some(AgentPayload::Heartbeat(value)) => format!("heartbeat from {}", value.agent_id),
        Some(AgentPayload::Interfaces(value)) => format!("interfaces from {}", value.node_id),
        Some(AgentPayload::XrayTraffic(value)) => format!("xray traffic from {}", value.node_id),
        Some(AgentPayload::DesiredStateAck(value)) => {
            format!("state ack revision {}", value.revision)
        }
        Some(AgentPayload::System(value)) => format!("system snapshot {}", value.sequence),
        None => "empty agent message".into(),
    }
}

fn desired_signature(users: &[xenon_storage::models::DesiredXrayUser]) -> String {
    users
        .iter()
        .map(|user| format!("{}={}", user.xray_email, user.xray_uuid))
        .collect::<Vec<_>>()
        .join("|")
}

fn desired_state_message(
    revision: u64,
    users: Vec<xenon_storage::models::DesiredXrayUser>,
) -> PanelToAgent {
    PanelToAgent {
        payload: Some(PanelPayload::DesiredState(DesiredStateSnapshot {
            revision,
            desired_users: users
                .into_iter()
                .map(|user| XrayUser {
                    subscription_id: user.subscription_id,
                    xray_email: user.xray_email,
                    uuid: user.xray_uuid,
                    enabled: true,
                })
                .collect(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_agent_protocol_and_embedded_xray_version() {
        assert!(validate_agent_compatibility(
            xenon_domain::PANEL_AGENT_PROTOCOL_VERSION,
            xenon_domain::MAX_XRAY_CORE_VERSION,
            "26.6.27 (sha256)"
        )
        .is_ok());
        assert!(validate_agent_compatibility(
            xenon_domain::PANEL_AGENT_PROTOCOL_VERSION,
            xenon_domain::MAX_XRAY_CORE_VERSION,
            "not-configured"
        )
        .is_ok());
        assert!(
            validate_agent_compatibility("0", xenon_domain::MAX_XRAY_CORE_VERSION, "26.6.27")
                .is_err()
        );
        assert!(validate_agent_compatibility(
            xenon_domain::PANEL_AGENT_PROTOCOL_VERSION,
            xenon_domain::MAX_XRAY_CORE_VERSION,
            "26.7.1"
        )
        .is_err());
    }

    #[test]
    fn issues_restricted_client_certificate_from_verified_csr() {
        let ca_key = KeyPair::generate().expect("CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "enrollment-test-ca");
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_certificate = ca_params.self_signed(&ca_key).expect("CA certificate");

        let agent_key = KeyPair::generate().expect("Agent key");
        let mut agent_params = CertificateParams::new(Vec::<String>::new()).expect("Agent params");
        agent_params
            .distinguished_name
            .push(DnType::CommonName, "untrusted-csr-name");
        let csr = agent_params
            .serialize_request(&agent_key)
            .expect("CSR")
            .pem()
            .expect("CSR PEM");

        let issued = issue_client_certificate(
            &csr,
            "agent-test",
            &ca_certificate.pem(),
            &ca_key.serialize_pem(),
            30,
        )
        .expect("issued certificate");
        assert_eq!(issued.fingerprint.len(), 64);
        assert_eq!(issued.public_key_sha256.len(), 64);
        assert!(issued.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(issued.expires_at > Utc::now().timestamp());
    }
}
