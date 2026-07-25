#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserSummary {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub status: String,
    pub subscription_count: i64,
    pub charged_bytes: i64,
    pub traffic_limit_bytes: Option<i64>,
    pub expired_subscriptions: i64,
}

#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct NicCounterTotals {
    pub rx_bytes: i64,
    pub tx_bytes: i64,
    pub sampled_at: i64,
}

#[derive(Debug, Clone)]
pub struct UserNodeUsage {
    pub subscription_id: String,
    pub subscription_name: String,
    pub node_id: String,
    pub node_name: String,
    pub uplink_bytes: i64,
    pub downlink_bytes: i64,
    pub charged_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct SubscriptionNicUsage {
    pub subscription_id: String,
    pub used_bytes: i64,
    pub limit_bytes: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrafficAggregate {
    pub granularity: String,
    pub subscription_id: String,
    pub node_id: String,
    pub bucket_start: i64,
    pub bucket_end: Option<i64>,
    pub uplink_bytes: i64,
    pub downlink_bytes: i64,
    pub event_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NicTrafficAggregate {
    pub granularity: String,
    pub node_id: String,
    pub interface_name: String,
    pub bucket_start: i64,
    pub bucket_end: i64,
    pub rx_bytes: i64,
    pub tx_bytes: i64,
    pub sample_count: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrafficPruneResult {
    pub xray_events: u64,
    pub interface_snapshots: u64,
    pub system_snapshots: u64,
    pub hourly_aggregates: u64,
    pub daily_aggregates: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseVerification {
    pub schema_version: i64,
    pub integrity_messages: Vec<String>,
    pub foreign_key_violations: i64,
    pub applied_migrations: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NicBindingRecord {
    pub id: String,
    pub subscription_id: String,
    pub node_id: String,
    pub interface_name: String,
    pub billing_direction: String,
    pub traffic_limit_bytes: i64,
    pub initial_used_bytes: i64,
    pub reset_policy: String,
    pub reset_anchor: Option<i64>,
    pub bound_at: i64,
    pub current_cycle_start: i64,
    pub current_cycle_end: Option<i64>,
    pub used_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct UserDetail {
    pub user: UserSummary,
    pub subscriptions: Vec<SubscriptionRecord>,
    pub node_usage: Vec<UserNodeUsage>,
    pub nic_usage: Vec<SubscriptionNicUsage>,
    pub nic_bindings: Vec<NicBindingRecord>,
}

#[derive(Debug, Clone)]
pub struct NewSubscription {
    pub user_id: String,
    pub username: String,
    pub subscription_id: String,
    pub name: String,
    pub token_hash: String,
    pub xray_uuid: String,
    pub xray_email: String,
    pub starts_at: i64,
    pub expires_at: Option<i64>,
    pub traffic_limit_bytes: Option<i64>,
    pub traffic_multiplier_basis_points: i64,
    pub reset_policy: String,
    pub reset_anchor: Option<i64>,
    pub current_cycle_start: i64,
    pub current_cycle_end: Option<i64>,
    pub node_ids: Vec<String>,
    pub nic_bindings: Vec<NewNicBinding>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct UpdateSubscription {
    pub name: String,
    pub status: String,
    pub expires_at: Option<i64>,
    pub traffic_limit_bytes: Option<i64>,
    pub traffic_multiplier_basis_points: i64,
    pub reset_policy: String,
    pub reset_anchor: Option<i64>,
    pub current_cycle_end: Option<i64>,
    pub node_ids: Vec<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewNicBinding {
    pub id: String,
    pub node_id: String,
    pub interface_name: String,
    pub billing_direction: String,
    pub traffic_limit_bytes: i64,
    pub initial_used_bytes: i64,
    pub reset_policy: String,
    pub reset_anchor: Option<i64>,
    pub current_cycle_start: i64,
    pub current_cycle_end: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SubscriptionRecord {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub xray_uuid: String,
    pub xray_email: String,
    pub status: String,
    pub starts_at: i64,
    pub expires_at: Option<i64>,
    pub traffic_limit_bytes: Option<i64>,
    pub traffic_multiplier_basis_points: i64,
    pub reset_policy: String,
    pub reset_anchor: Option<i64>,
    pub current_cycle_start: i64,
    pub current_cycle_end: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NodeRecord {
    pub id: String,
    pub name: String,
    pub landing_host: String,
    pub xray_listen_port: i64,
    pub publish_host: Option<String>,
    pub publish_port: Option<i64>,
    pub protocol: String,
    pub transport: String,
    pub security: String,
    pub server_name: Option<String>,
    pub reality_public_key: Option<String>,
    pub reality_short_id: Option<String>,
    pub reality_fingerprint: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NodeOverview {
    pub id: String,
    pub name: String,
    pub landing_host: String,
    pub xray_listen_port: i64,
    pub publish_host: Option<String>,
    pub publish_port: Option<i64>,
    pub protocol: String,
    pub transport: String,
    pub security: String,
    pub server_name: Option<String>,
    pub reality_public_key: Option<String>,
    pub reality_short_id: Option<String>,
    pub reality_fingerprint: Option<String>,
    pub node_status: String,
    pub management_status: String,
    pub desired_revision: i64,
    pub agent_status: Option<String>,
    pub last_seen_at: Option<i64>,
    pub agent_version: Option<String>,
    pub xray_version: Option<String>,
    pub cpu_usage_basis_points: Option<i64>,
    pub load_1_milli: Option<i64>,
    pub memory_total_bytes: Option<i64>,
    pub memory_used_bytes: Option<i64>,
    pub disk_total_bytes: Option<i64>,
    pub disk_used_bytes: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct UpdateNode {
    pub name: String,
    pub landing_host: String,
    pub xray_listen_port: i64,
    pub publish_host: Option<String>,
    pub publish_port: Option<i64>,
    pub security: String,
    pub server_name: Option<String>,
    pub reality_public_key: Option<String>,
    pub reality_short_id: Option<String>,
    pub reality_fingerprint: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewRegistrationToken {
    pub id: String,
    pub node_id: String,
    pub token_hash: String,
    pub expires_at: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct EnrollmentCertificate {
    pub fingerprint_sha256: String,
    pub certificate_pem: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewNode {
    pub id: String,
    pub name: String,
    pub landing_host: String,
    pub xray_listen_port: i64,
    pub publish_host: Option<String>,
    pub publish_port: Option<i64>,
    pub protocol: String,
    pub transport: String,
    pub security: String,
    pub server_name: Option<String>,
    pub reality_public_key: Option<String>,
    pub reality_short_id: Option<String>,
    pub reality_fingerprint: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DesiredXrayUser {
    pub subscription_id: String,
    pub xray_email: String,
    pub xray_uuid: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InterfaceRecord {
    pub node_id: String,
    pub interface_name: String,
    pub sampled_at: i64,
}
