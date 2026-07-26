use crate::{config::SubscriptionHttpConfig, secrets::sha256_hex, RuntimeState};
use axum::{
    extract::{ConnectInfo, Path, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::Utc;
use serde::Serialize;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::RwLock};
use url::Url;
use xenon_storage::{models::ProxyNodeRecord, Database};

#[derive(Clone)]
struct HttpState {
    runtime: Arc<RwLock<RuntimeState>>,
    database: Database,
    rate_limiter: Arc<RateLimiter>,
    agent_ca_pem: Option<Arc<Vec<u8>>>,
    agent_bootstrap: Option<Arc<Vec<u8>>>,
}

pub async fn serve(
    addr: String,
    config: SubscriptionHttpConfig,
    runtime: Arc<RwLock<RuntimeState>>,
    database: Database,
    agent_ca_pem: Option<Vec<u8>>,
    agent_bootstrap_manifest: Option<Vec<u8>>,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/agent-ca.crt", get(agent_ca))
        .route("/agent-bootstrap", get(agent_bootstrap))
        .route("/sub/:token", get(subscription))
        .route("/sub/:token/vless", get(subscription_vless))
        .route("/sub/:token/mihomo", get(subscription_mihomo))
        .route("/sub/:token/sing-box", get(subscription_sing_box))
        .with_state(HttpState {
            runtime,
            database,
            agent_ca_pem: agent_ca_pem.map(Arc::new),
            agent_bootstrap: agent_bootstrap_manifest.map(Arc::new),
            rate_limiter: Arc::new(RateLimiter::new(
                config.requests_per_minute_per_ip,
                config.requests_per_minute_per_token,
            )),
        });
    let socket_addr: SocketAddr = addr.parse()?;
    if config.tls_enabled {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &config.cert_path,
            &config.key_path,
        )
        .await?;
        tracing::info!(%addr, "subscription HTTPS server listening");
        axum_server::bind_rustls(socket_addr, tls)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = TcpListener::bind(socket_addr).await?;
        tracing::info!(%addr, "subscription HTTP server listening");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
    }
    Ok(())
}

async fn agent_ca(State(state): State<HttpState>) -> Response {
    let Some(pem) = state.agent_ca_pem else {
        return (StatusCode::NOT_FOUND, "Agent CA is not configured\n").into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-pem-file"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, pem.as_ref().clone()).into_response()
}

async fn agent_bootstrap(State(state): State<HttpState>) -> Response {
    let Some(manifest) = state.agent_bootstrap else {
        return (StatusCode::NOT_FOUND, "Agent bootstrap is not configured\n").into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, manifest.as_ref().clone()).into_response()
}

async fn healthz(State(state): State<HttpState>) -> impl IntoResponse {
    let runtime = state.runtime.read().await;
    (
        StatusCode::OK,
        format!("ok\nconnected_agents={}\n", runtime.connected_agents),
    )
}

async fn subscription(
    Path(token): Path<String>,
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    subscription_response(token, state, SubscriptionFormat::Vless, peer.ip()).await
}

async fn subscription_vless(
    Path(token): Path<String>,
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    subscription_response(token, state, SubscriptionFormat::Vless, peer.ip()).await
}

async fn subscription_mihomo(
    Path(token): Path<String>,
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    subscription_response(token, state, SubscriptionFormat::Mihomo, peer.ip()).await
}

async fn subscription_sing_box(
    Path(token): Path<String>,
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    subscription_response(token, state, SubscriptionFormat::SingBox, peer.ip()).await
}

#[derive(Debug, Clone, Copy)]
enum SubscriptionFormat {
    Vless,
    Mihomo,
    SingBox,
}

impl SubscriptionFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Vless => "vless",
            Self::Mihomo => "mihomo",
            Self::SingBox => "sing-box",
        }
    }
}

async fn subscription_response(
    token: String,
    state: HttpState,
    format: SubscriptionFormat,
    client_ip: IpAddr,
) -> Response {
    let token_hash = sha256_hex(token.as_bytes());
    let token_hash_prefix = &token_hash[..12];
    let response = if state.rate_limiter.allow(client_ip, &token_hash) {
        subscription_response_by_hash(&token_hash, &state, format).await
    } else {
        rate_limited()
    };
    tracing::info!(
        %client_ip,
        token_hash_prefix,
        format = format.name(),
        status = response.status().as_u16(),
        "subscription request"
    );
    response
}

async fn subscription_response_by_hash(
    token_hash: &str,
    state: &HttpState,
    format: SubscriptionFormat,
) -> Response {
    let now = Utc::now().timestamp();
    if let Err(error) = state.database.advance_billing_cycles(now).await {
        return storage_error(error);
    }
    let record = match state.database.subscription_by_token_hash(token_hash).await {
        Ok(Some(record)) => record,
        Ok(None) => return not_found(),
        Err(error) => return storage_error(error),
    };
    if record.starts_at > now || record.expires_at.is_some_and(|expires| expires <= now) {
        return not_found();
    }
    let nodes = match state.database.subscription_proxy_nodes(&record.id).await {
        Ok(nodes) if !nodes.is_empty() => nodes,
        Ok(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "subscription has no available nodes\n",
            )
                .into_response()
        }
        Err(error) => return storage_error(error),
    };
    let (uplink, downlink) = match state
        .database
        .subscription_xray_usage(&record.id, record.current_cycle_start)
        .await
    {
        Ok(usage) => usage,
        Err(error) => return storage_error(error),
    };
    let (upload, download, total) = match state.database.subscription_nic_usage(&record.id).await {
        Ok(Some((used, limit))) => (0, used as u64, limit as u64),
        Ok(None) => (
            charged_bytes(uplink, record.traffic_multiplier_basis_points),
            charged_bytes(downlink, record.traffic_multiplier_basis_points),
            record.traffic_limit_bytes.unwrap_or_default().max(0) as u64,
        ),
        Err(error) => return storage_error(error),
    };
    let client_nodes = nodes
        .iter()
        .map(|node| ClientNode::from_record(node, &record.xray_uuid))
        .collect::<Vec<_>>();
    let body = match render_subscription(format, &client_nodes) {
        Ok(body) => body,
        Err(error) => return render_error(error),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(match format {
            SubscriptionFormat::Vless => "text/plain; charset=utf-8",
            SubscriptionFormat::Mihomo => "application/yaml; charset=utf-8",
            SubscriptionFormat::SingBox => "application/json; charset=utf-8",
        }),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("profile-update-interval"),
        HeaderValue::from_static("6"),
    );
    let userinfo = userinfo_header(upload, download, total, record.expires_at);
    if let Ok(value) = HeaderValue::from_str(&userinfo) {
        headers.insert(HeaderName::from_static("subscription-userinfo"), value);
    }
    (StatusCode::OK, headers, body).into_response()
}

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

struct RateLimiter {
    state: Mutex<RateLimitState>,
    per_ip: u32,
    per_token: u32,
}

#[derive(Default)]
struct RateLimitState {
    ips: HashMap<IpAddr, RequestWindow>,
    tokens: HashMap<String, RequestWindow>,
}

struct RequestWindow {
    started_at: Instant,
    requests: u32,
}

impl RateLimiter {
    fn new(per_ip: u32, per_token: u32) -> Self {
        Self {
            state: Mutex::new(RateLimitState::default()),
            per_ip,
            per_token,
        }
    }

    fn allow(&self, ip: IpAddr, token_hash: &str) -> bool {
        let now = Instant::now();
        let mut state = self.state.lock().expect("rate limiter mutex poisoned");
        state
            .ips
            .retain(|_, window| now.duration_since(window.started_at) < RATE_LIMIT_WINDOW);
        state
            .tokens
            .retain(|_, window| now.duration_since(window.started_at) < RATE_LIMIT_WINDOW);
        if !consume_window(&mut state.ips, ip, self.per_ip, now) {
            return false;
        }
        consume_window(
            &mut state.tokens,
            token_hash.to_string(),
            self.per_token,
            now,
        )
    }
}

fn consume_window<K: Eq + std::hash::Hash>(
    windows: &mut HashMap<K, RequestWindow>,
    key: K,
    limit: u32,
    now: Instant,
) -> bool {
    let window = windows.entry(key).or_insert(RequestWindow {
        started_at: now,
        requests: 0,
    });
    if window.requests >= limit {
        return false;
    }
    window.requests += 1;
    true
}

#[derive(Debug, Clone)]
struct ClientNode {
    name: String,
    server: String,
    port: u16,
    uuid: String,
    transport: String,
    security: String,
    websocket_path: Option<String>,
    vless_encryption: Option<String>,
    server_name: Option<String>,
    reality_public_key: Option<String>,
    reality_short_id: Option<String>,
    reality_fingerprint: Option<String>,
}

impl ClientNode {
    fn from_record(node: &ProxyNodeRecord, uuid: &str) -> Self {
        Self {
            name: node.name.clone(),
            server: node
                .publish_host
                .clone()
                .unwrap_or_else(|| node.landing_host.clone()),
            port: node.publish_port.unwrap_or(node.listen_port) as u16,
            uuid: uuid.to_string(),
            transport: node.transport.clone(),
            security: node.security.clone(),
            websocket_path: node.websocket_path.clone(),
            vless_encryption: node.vless_encryption.clone(),
            server_name: node.server_name.clone(),
            reality_public_key: node.reality_public_key.clone(),
            reality_short_id: node.reality_short_id.clone(),
            reality_fingerprint: node.reality_fingerprint.clone(),
        }
    }
}

fn render_subscription(format: SubscriptionFormat, nodes: &[ClientNode]) -> anyhow::Result<String> {
    match format {
        SubscriptionFormat::Vless => Ok(STANDARD.encode(
            nodes
                .iter()
                .map(vless_link)
                .collect::<anyhow::Result<Vec<_>>>()?
                .join("\n"),
        )),
        SubscriptionFormat::Mihomo => Ok(serde_yaml::to_string(&MihomoConfig::new(nodes))?),
        SubscriptionFormat::SingBox => {
            Ok(serde_json::to_string_pretty(&SingBoxConfig::new(nodes))?)
        }
    }
}

#[derive(Serialize)]
struct MihomoConfig {
    proxies: Vec<MihomoProxy>,
    #[serde(rename = "proxy-groups")]
    proxy_groups: Vec<MihomoProxyGroup>,
}

impl MihomoConfig {
    fn new(nodes: &[ClientNode]) -> Self {
        Self {
            proxies: nodes.iter().map(MihomoProxy::from).collect(),
            proxy_groups: vec![MihomoProxyGroup {
                name: "Proxy".into(),
                kind: "select",
                proxies: nodes.iter().map(|node| node.name.clone()).collect(),
            }],
        }
    }
}

#[derive(Serialize)]
struct MihomoProxy {
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    server: String,
    port: u16,
    uuid: String,
    network: String,
    tls: bool,
    udp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    servername: Option<String>,
    #[serde(rename = "reality-opts", skip_serializing_if = "Option::is_none")]
    reality_opts: Option<MihomoRealityOptions>,
}

impl From<&ClientNode> for MihomoProxy {
    fn from(node: &ClientNode) -> Self {
        Self {
            name: node.name.clone(),
            kind: "vless",
            server: node.server.clone(),
            port: node.port,
            uuid: node.uuid.clone(),
            network: node.transport.clone(),
            tls: node.security != "none",
            udp: true,
            servername: node.server_name.clone(),
            reality_opts: (node.security == "reality").then(|| MihomoRealityOptions {
                public_key: node.reality_public_key.clone().unwrap_or_default(),
                short_id: node.reality_short_id.clone().unwrap_or_default(),
            }),
        }
    }
}

#[derive(Serialize)]
struct MihomoRealityOptions {
    #[serde(rename = "public-key")]
    public_key: String,
    #[serde(rename = "short-id")]
    short_id: String,
}

#[derive(Serialize)]
struct MihomoProxyGroup {
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    proxies: Vec<String>,
}

#[derive(Serialize)]
struct SingBoxConfig {
    outbounds: Vec<SingBoxOutbound>,
}

impl SingBoxConfig {
    fn new(nodes: &[ClientNode]) -> Self {
        Self {
            outbounds: nodes.iter().map(SingBoxOutbound::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct SingBoxOutbound {
    #[serde(rename = "type")]
    kind: &'static str,
    tag: String,
    server: String,
    server_port: u16,
    uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: Option<SingBoxTls>,
}

impl From<&ClientNode> for SingBoxOutbound {
    fn from(node: &ClientNode) -> Self {
        Self {
            kind: "vless",
            tag: node.name.clone(),
            server: node.server.clone(),
            server_port: node.port,
            uuid: node.uuid.clone(),
            tls: (node.security != "none").then(|| SingBoxTls {
                enabled: true,
                server_name: node.server_name.clone(),
                reality: (node.security == "reality").then(|| SingBoxReality {
                    enabled: true,
                    public_key: node.reality_public_key.clone().unwrap_or_default(),
                    short_id: node.reality_short_id.clone().unwrap_or_default(),
                }),
            }),
        }
    }
}

#[derive(Serialize)]
struct SingBoxTls {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reality: Option<SingBoxReality>,
}

#[derive(Serialize)]
struct SingBoxReality {
    enabled: bool,
    public_key: String,
    short_id: String,
}

fn charged_bytes(raw: i64, basis_points: i64) -> u64 {
    let raw = raw.max(0) as u128;
    let multiplier = basis_points.max(0) as u128;
    let charged = raw.saturating_mul(multiplier) / 10_000;
    charged.min(u64::MAX as u128) as u64
}

fn userinfo_header(upload: u64, download: u64, total: u64, expires_at: Option<i64>) -> String {
    let mut value = format!("upload={upload}; download={download}; total={total}");
    if let Some(expires_at) = expires_at.filter(|value| *value > 0) {
        value.push_str(&format!("; expire={expires_at}"));
    }
    value
}

fn vless_link(node: &ClientNode) -> anyhow::Result<String> {
    let host = if node.server.contains(':') && !node.server.starts_with('[') {
        format!("[{}]", node.server)
    } else {
        node.server.clone()
    };
    let mut url = Url::parse(&format!("vless://{}@{host}:{}", node.uuid, node.port))?;
    url.query_pairs_mut()
        .append_pair(
            "encryption",
            node.vless_encryption.as_deref().unwrap_or("none"),
        )
        .append_pair("type", &node.transport)
        .append_pair("security", &node.security);
    if node.transport == "ws" {
        if let Some(path) = &node.websocket_path {
            url.query_pairs_mut().append_pair("path", path);
        }
    }
    if let Some(server_name) = &node.server_name {
        url.query_pairs_mut().append_pair("sni", server_name);
    }
    if node.security == "reality" {
        if let Some(public_key) = &node.reality_public_key {
            url.query_pairs_mut().append_pair("pbk", public_key);
        }
        if let Some(short_id) = &node.reality_short_id {
            url.query_pairs_mut().append_pair("sid", short_id);
        }
        if let Some(fingerprint) = &node.reality_fingerprint {
            url.query_pairs_mut().append_pair("fp", fingerprint);
        }
    }
    url.set_fragment(Some(&node.name));
    Ok(url.into())
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "subscription not found\n").into_response()
}

fn rate_limited() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (
        StatusCode::TOO_MANY_REQUESTS,
        headers,
        "subscription request rate exceeded\n",
    )
        .into_response()
}

fn storage_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "subscription storage failure");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "subscription unavailable\n",
    )
        .into_response()
}

fn render_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "subscription rendering failure");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "subscription unavailable\n",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        agent_bootstrap, agent_ca, charged_bytes, render_subscription, subscription_response,
        userinfo_header, ClientNode, HttpState, RateLimiter, SubscriptionFormat,
    };
    use crate::{secrets::sha256_hex, RuntimeState};
    use axum::{body::to_bytes, http::StatusCode};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::{net::IpAddr, sync::Arc};
    use tokio::sync::RwLock;
    use xenon_storage::{models::NewSubscription, Database};

    #[test]
    fn renders_publish_address_and_billing_header() {
        let node = ClientNode {
            name: "Hong Kong 01".into(),
            server: "relay.example.com".into(),
            port: 8443,
            uuid: "01900000-0000-7000-8000-000000000001".into(),
            transport: "ws".into(),
            security: "none".into(),
            websocket_path: Some("/xray".into()),
            vless_encryption: Some("test-encryption".into()),
            server_name: None,
            reality_public_key: None,
            reality_short_id: None,
            reality_fingerprint: None,
        };
        let link = render_subscription(SubscriptionFormat::Vless, std::slice::from_ref(&node))
            .expect("vless output");
        let link = String::from_utf8(STANDARD.decode(link).expect("base64")).expect("text");
        assert!(link.contains("@relay.example.com:8443"));
        assert!(link.contains("encryption=test-encryption"));
        assert!(link.contains("path=%2Fxray"));
        assert!(link.ends_with("#Hong%20Kong%2001"));
        let mihomo = render_subscription(SubscriptionFormat::Mihomo, std::slice::from_ref(&node))
            .expect("mihomo output");
        assert!(mihomo.contains("type: vless"));
        let sing_box =
            render_subscription(SubscriptionFormat::SingBox, &[node]).expect("sing-box output");
        assert!(sing_box.contains("\"type\": \"vless\""));
        let reality = ClientNode {
            name: "Reality Node".into(),
            server: "edge.example.com".into(),
            port: 443,
            uuid: "01900000-0000-7000-8000-000000000002".into(),
            transport: "tcp".into(),
            security: "reality".into(),
            websocket_path: None,
            vless_encryption: None,
            server_name: Some("www.example.com".into()),
            reality_public_key: Some("public-key".into()),
            reality_short_id: Some("short".into()),
            reality_fingerprint: Some("chrome".into()),
        };
        let reality_vless =
            render_subscription(SubscriptionFormat::Vless, std::slice::from_ref(&reality))
                .expect("reality vless");
        let reality_vless =
            String::from_utf8(STANDARD.decode(reality_vless).expect("base64")).expect("text");
        assert!(reality_vless.contains("security=reality"));
        assert!(reality_vless.contains("pbk=public-key"));
        let reality_mihomo =
            render_subscription(SubscriptionFormat::Mihomo, std::slice::from_ref(&reality))
                .expect("reality mihomo");
        assert!(reality_mihomo.contains("reality-opts"));
        assert_eq!(charged_bytes(25, 20_000), 50);
        assert_eq!(
            userinfo_header(10, 20, 100, Some(200)),
            "upload=10; download=20; total=100; expire=200"
        );
    }

    #[tokio::test]
    async fn serves_subscription_from_hashed_token() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        database
            .ensure_development_node("node-a", 1)
            .await
            .expect("node");
        sqlx::query(
            "UPDATE nodes SET name = 'Relay Node', publish_host = 'relay.example.com',
             publish_port = 8443 WHERE id = 'node-a'",
        )
        .execute(database.pool())
        .await
        .expect("update node");
        sqlx::query(
            "UPDATE proxy_nodes SET name = 'Relay Node', publish_host = 'relay.example.com',
             publish_port = 8443 WHERE id = 'node-a'",
        )
        .execute(database.pool())
        .await
        .expect("update proxy node");
        database
            .create_user_subscription(&NewSubscription {
                user_id: "user-a".into(),
                username: "alice".into(),
                subscription_id: "subscription-a".into(),
                name: "Alice".into(),
                token_hash: sha256_hex(b"plain-secret-token"),
                xray_uuid: "01900000-0000-7000-8000-000000000001".into(),
                xray_email: "sub-subscription-a@panel".into(),
                starts_at: 1,
                expires_at: None,
                traffic_limit_bytes: Some(1000),
                traffic_multiplier_basis_points: 20_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-a".into()],
                nic_bindings: Vec::new(),
                created_at: 1,
            })
            .await
            .expect("subscription");
        let state = HttpState {
            runtime: Arc::new(RwLock::new(RuntimeState::default())),
            database,
            rate_limiter: Arc::new(RateLimiter::new(120, 60)),
            agent_ca_pem: None,
            agent_bootstrap: None,
        };
        let client_ip: IpAddr = "127.0.0.1".parse().expect("client IP");
        let response = subscription_response(
            "plain-secret-token".into(),
            state.clone(),
            SubscriptionFormat::Vless,
            client_ip,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["subscription-userinfo"],
            "upload=0; download=0; total=1000"
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let decoded = STANDARD.decode(bytes).expect("base64");
        let text = String::from_utf8(decoded).expect("utf8");
        assert!(text.contains("@relay.example.com:8443"));

        sqlx::query(
            "INSERT INTO nic_bindings
                (id, subscription_id, node_id, interface_name, traffic_limit_bytes,
                 initial_used_bytes, reset_policy, bound_at)
             VALUES ('binding-a', 'subscription-a', 'node-a', 'eth0', 5000, 100, 'never', 2)",
        )
        .execute(state.database.pool())
        .await
        .expect("NIC binding");
        state
            .database
            .insert_interface_snapshots("node-a", "boot-a", 1, 1, &[("eth0".into(), 60, 40)])
            .await
            .expect("baseline interface snapshot");
        state
            .database
            .insert_interface_snapshots("node-a", "boot-a", 2, 3, &[("eth0".into(), 220, 180)])
            .await
            .expect("current interface snapshot");
        let bound_response = subscription_response(
            "plain-secret-token".into(),
            state,
            SubscriptionFormat::Vless,
            client_ip,
        )
        .await;
        assert_eq!(
            bound_response.headers()["subscription-userinfo"],
            "upload=0; download=400; total=5000"
        );
    }

    #[test]
    fn limits_both_source_ip_and_token_without_storing_plain_tokens() {
        let ip_limiter = RateLimiter::new(2, 10);
        let first_ip: IpAddr = "192.0.2.1".parse().expect("first IP");
        assert!(ip_limiter.allow(first_ip, "hash-a"));
        assert!(ip_limiter.allow(first_ip, "hash-b"));
        assert!(!ip_limiter.allow(first_ip, "hash-c"));

        let token_limiter = RateLimiter::new(10, 2);
        let second_ip: IpAddr = "192.0.2.2".parse().expect("second IP");
        let third_ip: IpAddr = "192.0.2.3".parse().expect("third IP");
        assert!(token_limiter.allow(first_ip, "shared-hash"));
        assert!(token_limiter.allow(second_ip, "shared-hash"));
        assert!(!token_limiter.allow(third_ip, "shared-hash"));
    }

    #[tokio::test]
    async fn serves_public_agent_ca() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        let state = HttpState {
            runtime: Arc::new(RwLock::new(RuntimeState::default())),
            database,
            rate_limiter: Arc::new(RateLimiter::new(120, 60)),
            agent_ca_pem: Some(Arc::new(b"-----BEGIN CERTIFICATE-----\ntest\n".to_vec())),
            agent_bootstrap: None,
        };
        let response = agent_ca(axum::extract::State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("CA body");
        assert_eq!(body, b"-----BEGIN CERTIFICATE-----\ntest\n".as_slice());
    }

    #[tokio::test]
    async fn serves_public_agent_bootstrap_manifest() {
        let temp = tempfile::tempdir().expect("temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("database");
        let state = HttpState {
            runtime: Arc::new(RwLock::new(RuntimeState::default())),
            database,
            rate_limiter: Arc::new(RateLimiter::new(120, 60)),
            agent_ca_pem: None,
            agent_bootstrap: Some(Arc::new(b"panel_endpoint=https://panel.test\n".to_vec())),
        };
        let response = agent_bootstrap(axum::extract::State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("bootstrap body");
        assert_eq!(body, b"panel_endpoint=https://panel.test\n".as_slice());
    }
}
