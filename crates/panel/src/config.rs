use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::Path};
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PanelConfig {
    #[serde(default = "default_grpc_addr")]
    pub grpc_addr: String,
    #[serde(default = "default_http_addr")]
    pub http_addr: String,
    #[serde(default = "default_database_path")]
    pub database_path: String,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub registration: RegistrationConfig,
    #[serde(default)]
    pub enrollment: EnrollmentConfig,
    #[serde(default)]
    pub agent_install: AgentInstallConfig,
    #[serde(default)]
    pub subscription_http: SubscriptionHttpConfig,
    #[serde(default)]
    pub traffic_retention: TrafficRetentionConfig,
    #[serde(default)]
    pub backup: BackupConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackupConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_backup_directory")]
    pub directory: String,
    #[serde(default = "default_backup_interval_hours")]
    pub interval_hours: u64,
    #[serde(default = "default_backup_retain_count")]
    pub retain_count: usize,
}

fn default_backup_directory() -> String {
    "data/backups".into()
}
fn default_backup_interval_hours() -> u64 {
    24
}
fn default_backup_retain_count() -> usize {
    7
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: default_backup_directory(),
            interval_hours: default_backup_interval_hours(),
            retain_count: default_backup_retain_count(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrafficRetentionConfig {
    #[serde(default = "default_maintenance_interval_seconds")]
    pub maintenance_interval_seconds: u64,
    #[serde(default = "default_raw_event_days")]
    pub raw_event_days: u32,
    #[serde(default = "default_interface_snapshot_days")]
    pub interface_snapshot_days: u32,
    #[serde(default = "default_system_snapshot_days")]
    pub system_snapshot_days: u32,
    #[serde(default)]
    pub hourly_aggregate_days: u32,
    #[serde(default)]
    pub daily_aggregate_days: u32,
}

fn default_maintenance_interval_seconds() -> u64 {
    3600
}
fn default_raw_event_days() -> u32 {
    30
}
fn default_interface_snapshot_days() -> u32 {
    30
}
fn default_system_snapshot_days() -> u32 {
    7
}

impl Default for TrafficRetentionConfig {
    fn default() -> Self {
        Self {
            maintenance_interval_seconds: default_maintenance_interval_seconds(),
            raw_event_days: default_raw_event_days(),
            interface_snapshot_days: default_interface_snapshot_days(),
            system_snapshot_days: default_system_snapshot_days(),
            hourly_aggregate_days: 0,
            daily_aggregate_days: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionHttpConfig {
    #[serde(default)]
    pub tls_enabled: bool,
    #[serde(default)]
    pub allow_public_plaintext: bool,
    #[serde(default = "default_subscription_cert_path")]
    pub cert_path: String,
    #[serde(default = "default_subscription_key_path")]
    pub key_path: String,
    #[serde(default)]
    pub public_base_url: String,
    #[serde(default = "default_requests_per_minute_per_ip")]
    pub requests_per_minute_per_ip: u32,
    #[serde(default = "default_requests_per_minute_per_token")]
    pub requests_per_minute_per_token: u32,
}

fn default_subscription_cert_path() -> String {
    "/etc/xenon/tls/subscription.crt".into()
}
fn default_subscription_key_path() -> String {
    "/etc/xenon/tls/subscription.key".into()
}
fn default_requests_per_minute_per_ip() -> u32 {
    120
}
fn default_requests_per_minute_per_token() -> u32 {
    60
}

impl Default for SubscriptionHttpConfig {
    fn default() -> Self {
        Self {
            tls_enabled: false,
            allow_public_plaintext: false,
            cert_path: default_subscription_cert_path(),
            key_path: default_subscription_key_path(),
            public_base_url: String::new(),
            requests_per_minute_per_ip: default_requests_per_minute_per_ip(),
            requests_per_minute_per_token: default_requests_per_minute_per_token(),
        }
    }
}

impl SubscriptionHttpConfig {
    pub fn public_base_url(&self, http_addr: &str) -> String {
        if self.public_base_url.is_empty() {
            let scheme = if self.tls_enabled { "https" } else { "http" };
            format!("{scheme}://{http_addr}")
        } else {
            self.public_base_url.trim_end_matches('/').to_string()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentInstallConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub script_url: String,
    #[serde(default)]
    pub binary_url: String,
    #[serde(default)]
    pub binary_sha256: String,
    #[serde(default)]
    pub binary_sha256_x86_64: String,
    #[serde(default)]
    pub binary_sha256_aarch64: String,
    #[serde(default)]
    pub binary_version: String,
    #[serde(default)]
    pub ca_url: String,
    #[serde(default)]
    pub ca_path: String,
    #[serde(default)]
    pub panel_endpoint: String,
    #[serde(default)]
    pub enrollment_endpoint: String,
    #[serde(default)]
    pub server_name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RegistrationConfig {
    #[serde(default)]
    pub allow_insecure_dev_token: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnrollmentConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_enrollment_addr")]
    pub addr: String,
    #[serde(default = "default_enrollment_ca_cert_path")]
    pub ca_cert_path: String,
    #[serde(default = "default_enrollment_ca_key_path")]
    pub ca_key_path: String,
    #[serde(default = "default_certificate_valid_days")]
    pub certificate_valid_days: u32,
}

fn default_enrollment_addr() -> String {
    "127.0.0.1:50052".into()
}
fn default_enrollment_ca_cert_path() -> String {
    "/etc/xenon/tls/clients-ca.crt".into()
}
fn default_enrollment_ca_key_path() -> String {
    "/etc/xenon/tls/clients-ca.key".into()
}
fn default_certificate_valid_days() -> u32 {
    90
}

impl Default for EnrollmentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            addr: default_enrollment_addr(),
            ca_cert_path: default_enrollment_ca_cert_path(),
            ca_key_path: default_enrollment_ca_key_path(),
            certificate_valid_days: default_certificate_valid_days(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tls_cert_path")]
    pub cert_path: String,
    #[serde(default = "default_tls_key_path")]
    pub key_path: String,
    #[serde(default = "default_tls_client_ca_path")]
    pub client_ca_path: String,
}

fn default_tls_cert_path() -> String {
    "/etc/xenon/tls/server.crt".into()
}
fn default_tls_key_path() -> String {
    "/etc/xenon/tls/server.key".into()
}
fn default_tls_client_ca_path() -> String {
    "/etc/xenon/tls/clients-ca.crt".into()
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: default_tls_cert_path(),
            key_path: default_tls_key_path(),
            client_ca_path: default_tls_client_ca_path(),
        }
    }
}

fn default_grpc_addr() -> String {
    "127.0.0.1:50051".into()
}
fn default_http_addr() -> String {
    "127.0.0.1:18181".into()
}
fn default_database_path() -> String {
    "data/xenon.db".into()
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            grpc_addr: default_grpc_addr(),
            http_addr: default_http_addr(),
            database_path: default_database_path(),
            tls: TlsConfig::default(),
            registration: RegistrationConfig::default(),
            enrollment: EnrollmentConfig::default(),
            agent_install: AgentInstallConfig::default(),
            subscription_http: SubscriptionHttpConfig::default(),
            traffic_retention: TrafficRetentionConfig::default(),
            backup: BackupConfig::default(),
        }
    }
}

impl PanelConfig {
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            let config = Self::default();
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let content = toml::to_string_pretty(&config)?;
            tokio::fs::write(path, content).await?;
            return Ok(config);
        }
        let content = tokio::fs::read_to_string(path).await?;
        Ok(toml::from_str(&content)?)
    }

    pub async fn validate(&self) -> anyhow::Result<()> {
        let grpc_addr: SocketAddr = self
            .grpc_addr
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid grpc_addr {}: {error}", self.grpc_addr))?;
        if !self.tls.enabled && !grpc_addr.ip().is_loopback() {
            anyhow::bail!("grpc_addr must be loopback when TLS is disabled");
        }
        if self.registration.allow_insecure_dev_token && !grpc_addr.ip().is_loopback() {
            anyhow::bail!("insecure development registration is only allowed on loopback");
        }
        let http_addr: SocketAddr = self
            .http_addr
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid http_addr {}: {error}", self.http_addr))?;
        if !self.subscription_http.tls_enabled
            && !self.subscription_http.allow_public_plaintext
            && !is_local_or_private(http_addr)
        {
            anyhow::bail!(
                "http_addr must be loopback or private when subscription HTTPS is disabled; \
                 set subscription_http.allow_public_plaintext = true to explicitly allow public HTTP"
            );
        }
        if self.subscription_http.tls_enabled {
            for path in [
                &self.subscription_http.cert_path,
                &self.subscription_http.key_path,
            ] {
                if !Path::new(path).is_file() {
                    anyhow::bail!("subscription TLS file does not exist: {path}");
                }
            }
        }
        for (name, value) in [
            (
                "requests_per_minute_per_ip",
                self.subscription_http.requests_per_minute_per_ip,
            ),
            (
                "requests_per_minute_per_token",
                self.subscription_http.requests_per_minute_per_token,
            ),
        ] {
            if value == 0 || value > 1_000_000 {
                anyhow::bail!("subscription_http {name} must be between 1 and 1000000");
            }
        }
        if self.backup.enabled {
            if self.backup.directory.trim().is_empty() {
                anyhow::bail!("backup directory must not be empty");
            }
            if !(1..=8_760).contains(&self.backup.interval_hours) {
                anyhow::bail!("backup interval_hours must be between 1 and 8760");
            }
            if !(1..=1_000).contains(&self.backup.retain_count) {
                anyhow::bail!("backup retain_count must be between 1 and 1000");
            }
        }
        if !self.subscription_http.public_base_url.is_empty() {
            let url = Url::parse(&self.subscription_http.public_base_url).map_err(|error| {
                anyhow::anyhow!("invalid subscription public_base_url: {error}")
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                anyhow::bail!(
                    "subscription public_base_url must be an HTTP(S) origin or base path"
                );
            }
            if self.subscription_http.tls_enabled && url.scheme() != "https" {
                anyhow::bail!("subscription public_base_url must use HTTPS when TLS is enabled");
            }
        }
        if !(60..=86_400).contains(&self.traffic_retention.maintenance_interval_seconds) {
            anyhow::bail!(
                "traffic_retention maintenance_interval_seconds must be between 60 and 86400"
            );
        }
        for (name, value) in [
            ("raw_event_days", self.traffic_retention.raw_event_days),
            (
                "interface_snapshot_days",
                self.traffic_retention.interface_snapshot_days,
            ),
            (
                "system_snapshot_days",
                self.traffic_retention.system_snapshot_days,
            ),
        ] {
            if value == 0 || value > 36_500 {
                anyhow::bail!("traffic_retention {name} must be between 1 and 36500");
            }
        }
        for (name, value) in [
            (
                "hourly_aggregate_days",
                self.traffic_retention.hourly_aggregate_days,
            ),
            (
                "daily_aggregate_days",
                self.traffic_retention.daily_aggregate_days,
            ),
        ] {
            if value > 36_500 {
                anyhow::bail!("traffic_retention {name} must be 0 or at most 36500");
            }
        }
        if self.tls.enabled {
            for path in [
                &self.tls.cert_path,
                &self.tls.key_path,
                &self.tls.client_ca_path,
            ] {
                if !Path::new(path).is_file() {
                    anyhow::bail!("TLS file does not exist: {path}");
                }
            }
        }
        if self.enrollment.enabled {
            if !self.tls.enabled {
                anyhow::bail!("Agent enrollment requires Panel TLS");
            }
            let _: SocketAddr = self.enrollment.addr.parse().map_err(|error| {
                anyhow::anyhow!("invalid enrollment addr {}: {error}", self.enrollment.addr)
            })?;
            if self.enrollment.certificate_valid_days == 0
                || self.enrollment.certificate_valid_days > 825
            {
                anyhow::bail!("enrollment certificate_valid_days must be between 1 and 825");
            }
            for path in [&self.enrollment.ca_cert_path, &self.enrollment.ca_key_path] {
                if !Path::new(path).is_file() {
                    anyhow::bail!("enrollment CA file does not exist: {path}");
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = tokio::fs::metadata(&self.enrollment.ca_key_path)
                    .await?
                    .permissions()
                    .mode();
                if mode & 0o077 != 0 {
                    anyhow::bail!(
                        "enrollment CA private key must not be group/world accessible: {}",
                        self.enrollment.ca_key_path
                    );
                }
            }
        }
        if self.agent_install.enabled {
            for (name, value) in [
                ("script_url", &self.agent_install.script_url),
                ("binary_url", &self.agent_install.binary_url),
                ("panel_endpoint", &self.agent_install.panel_endpoint),
                (
                    "enrollment_endpoint",
                    &self.agent_install.enrollment_endpoint,
                ),
            ] {
                if !value.starts_with("https://")
                    || value.contains(char::is_whitespace)
                    || value.contains(['\"', '\'', '\\'])
                {
                    anyhow::bail!("agent_install {name} must be a safe HTTPS URL");
                }
            }
            if !self.agent_install.ca_url.is_empty()
                && (!self.agent_install.ca_url.starts_with("https://")
                    || self.agent_install.ca_url.contains(char::is_whitespace)
                    || self.agent_install.ca_url.contains(['\"', '\'', '\\']))
            {
                anyhow::bail!("agent_install ca_url must be a safe HTTPS URL");
            }
            if self.agent_install.ca_url.is_empty() && self.agent_install.ca_path.is_empty() {
                anyhow::bail!("agent_install needs ca_url or ca_path");
            }
            let valid_hash = |value: &str| {
                value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            };
            let generic_hash = valid_hash(&self.agent_install.binary_sha256);
            let architecture_hashes = valid_hash(&self.agent_install.binary_sha256_x86_64)
                && valid_hash(&self.agent_install.binary_sha256_aarch64);
            if !generic_hash && !architecture_hashes {
                anyhow::bail!(
                    "agent_install needs binary_sha256 or both architecture-specific SHA-256 values"
                );
            }
            if self.agent_install.binary_version.is_empty()
                || self.agent_install.binary_version.len() > 64
                || !self
                    .agent_install
                    .binary_version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
            {
                anyhow::bail!("agent_install binary_version is invalid");
            }
            if self.agent_install.server_name.is_empty()
                || !self
                    .agent_install
                    .server_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            {
                anyhow::bail!("agent_install server_name is invalid");
            }
        }
        Ok(())
    }
}

fn is_local_or_private(addr: SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00 == 0xfc00),
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentInstallConfig, PanelConfig};

    #[tokio::test]
    async fn rejects_public_plaintext_subscription_listener() {
        let config = PanelConfig {
            http_addr: "203.0.113.1:18081".into(),
            ..PanelConfig::default()
        };
        let error = config.validate().await.expect_err("public plaintext");
        assert!(error.to_string().contains("loopback or private"));
    }

    #[tokio::test]
    async fn accepts_explicitly_allowed_public_plaintext_subscription_listener() {
        let mut config = PanelConfig {
            http_addr: "0.0.0.0:18181".into(),
            ..PanelConfig::default()
        };
        config.subscription_http.allow_public_plaintext = true;
        config
            .validate()
            .await
            .expect("explicit public plaintext opt-in");
    }

    #[tokio::test]
    async fn validates_subscription_limits_and_builds_public_url() {
        let mut config = PanelConfig::default();
        assert_eq!(
            config.subscription_http.public_base_url(&config.http_addr),
            "http://127.0.0.1:18181"
        );
        config.subscription_http.public_base_url = "https://sub.example.com/base/".into();
        assert_eq!(
            config.subscription_http.public_base_url(&config.http_addr),
            "https://sub.example.com/base"
        );
        config.subscription_http.requests_per_minute_per_token = 0;
        let error = config.validate().await.expect_err("zero rate limit");
        assert!(error.to_string().contains("requests_per_minute_per_token"));
    }

    #[tokio::test]
    async fn accepts_architecture_specific_agent_hashes() {
        let config = PanelConfig {
            agent_install: AgentInstallConfig {
                enabled: true,
                script_url: "https://example.com/install-agent.sh".into(),
                binary_url: "https://example.com/xenon-agent-{arch}".into(),
                binary_sha256_x86_64: "a".repeat(64),
                binary_sha256_aarch64: "b".repeat(64),
                binary_version: "0.1.0-alpha.10".into(),
                ca_path: "/etc/xenon/tls/server-ca.crt".into(),
                panel_endpoint: "https://panel.example.com:50051".into(),
                enrollment_endpoint: "https://panel.example.com:50052".into(),
                server_name: "panel.example.com".into(),
                ..AgentInstallConfig::default()
            },
            ..PanelConfig::default()
        };
        config.validate().await.expect("agent install config");
    }
}
