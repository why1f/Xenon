use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    #[serde(default = "default_panel_endpoint")]
    pub panel_endpoint: String,
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    #[serde(default = "default_node_id")]
    pub node_id: String,
    #[serde(default = "default_registration_token")]
    pub registration_token: String,
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub xray: XrayConfig,
    #[serde(default)]
    pub spool: SpoolConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct XrayConfig {
    #[serde(default = "default_xray_api_endpoint")]
    pub api_endpoint: String,
    #[serde(default = "default_xray_inbound_tag")]
    pub inbound_tag: String,
    #[serde(default = "default_xray_listen_address")]
    pub listen_address: String,
    #[serde(default = "default_xray_listen_port")]
    pub listen_port: u16,
    #[serde(default = "default_xray_protocol")]
    pub protocol: String,
    #[serde(default = "default_xray_transport")]
    pub transport: String,
    #[serde(default = "default_xray_security")]
    pub security: String,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub tls_certificate_path: Option<String>,
    #[serde(default)]
    pub tls_key_path: Option<String>,
    #[serde(default)]
    pub reality_private_key: Option<String>,
    #[serde(default = "default_reality_dest")]
    pub reality_dest: String,
    #[serde(default = "default_reality_short_ids")]
    pub reality_short_ids: Vec<String>,
    #[serde(default = "default_reality_fingerprint")]
    pub reality_fingerprint: String,
    #[serde(default)]
    pub bootstrap_json: Option<String>,
}

fn default_xray_api_endpoint() -> String {
    "http://127.0.0.1:10085".into()
}

fn default_xray_inbound_tag() -> String {
    "vless-in".into()
}

fn default_xray_listen_address() -> String {
    "0.0.0.0".into()
}

fn default_xray_listen_port() -> u16 {
    443
}

fn default_xray_protocol() -> String {
    "vless".into()
}

fn default_xray_transport() -> String {
    "tcp".into()
}

fn default_xray_security() -> String {
    "none".into()
}

fn default_reality_dest() -> String {
    "www.cloudflare.com:443".into()
}

fn default_reality_short_ids() -> Vec<String> {
    vec!["".into()]
}

fn default_reality_fingerprint() -> String {
    "chrome".into()
}

impl Default for XrayConfig {
    fn default() -> Self {
        Self {
            api_endpoint: default_xray_api_endpoint(),
            inbound_tag: default_xray_inbound_tag(),
            listen_address: default_xray_listen_address(),
            listen_port: default_xray_listen_port(),
            protocol: default_xray_protocol(),
            transport: default_xray_transport(),
            security: default_xray_security(),
            server_name: None,
            tls_certificate_path: None,
            tls_key_path: None,
            reality_private_key: None,
            reality_dest: default_reality_dest(),
            reality_short_ids: default_reality_short_ids(),
            reality_fingerprint: default_reality_fingerprint(),
            bootstrap_json: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpoolConfig {
    #[serde(default = "default_spool_path")]
    pub path: String,
    #[serde(default = "default_spool_max_batches")]
    pub max_batches: usize,
    #[serde(default = "default_spool_max_bytes")]
    pub max_bytes: usize,
}

fn default_spool_path() -> String {
    "/var/lib/xenon/agent/traffic-spool.json".into()
}

fn default_spool_max_batches() -> usize {
    2048
}

fn default_spool_max_bytes() -> usize {
    16 * 1024 * 1024
}

impl Default for SpoolConfig {
    fn default() -> Self {
        Self {
            path: default_spool_path(),
            max_batches: default_spool_max_batches(),
            max_bytes: default_spool_max_bytes(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tls_ca_path")]
    pub ca_path: String,
    #[serde(default = "default_tls_cert_path")]
    pub cert_path: String,
    #[serde(default = "default_tls_key_path")]
    pub key_path: String,
    #[serde(default = "default_tls_domain")]
    pub domain_name: String,
    #[serde(default = "default_enrollment_endpoint")]
    pub enrollment_endpoint: String,
    #[serde(default)]
    pub certificate_expires_at: Option<i64>,
    #[serde(default = "default_renew_before_days")]
    pub renew_before_days: u32,
}

fn default_tls_ca_path() -> String {
    "/etc/xenon/tls/panel-ca.crt".into()
}
fn default_tls_cert_path() -> String {
    "/etc/xenon/tls/agent.crt".into()
}
fn default_tls_key_path() -> String {
    "/etc/xenon/tls/agent.key".into()
}
fn default_tls_domain() -> String {
    "panel.internal".into()
}
fn default_enrollment_endpoint() -> String {
    "https://127.0.0.1:50052".into()
}
fn default_renew_before_days() -> u32 {
    14
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ca_path: default_tls_ca_path(),
            cert_path: default_tls_cert_path(),
            key_path: default_tls_key_path(),
            domain_name: default_tls_domain(),
            enrollment_endpoint: default_enrollment_endpoint(),
            certificate_expires_at: None,
            renew_before_days: default_renew_before_days(),
        }
    }
}

fn default_panel_endpoint() -> String {
    "http://127.0.0.1:50051".into()
}
fn default_agent_id() -> String {
    "agent-dev".into()
}
fn default_node_id() -> String {
    "node-dev".into()
}
fn default_registration_token() -> String {
    "development-only".into()
}
fn default_interval_seconds() -> u64 {
    10
}

impl AgentConfig {
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            let config = Self {
                panel_endpoint: default_panel_endpoint(),
                agent_id: default_agent_id(),
                node_id: default_node_id(),
                registration_token: default_registration_token(),
                interval_seconds: default_interval_seconds(),
                tls: TlsConfig::default(),
                xray: XrayConfig::default(),
                spool: SpoolConfig::default(),
            };
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(path, toml::to_string_pretty(&config)?).await?;
            return Ok(config);
        }
        Ok(toml::from_str(&tokio::fs::read_to_string(path).await?)?)
    }

    pub async fn clear_registration_token(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        if !self.tls.enabled
            || self.registration_token.is_empty()
            || self.registration_token == "development-only"
        {
            return Ok(());
        }
        self.registration_token.clear();
        self.persist(path).await
    }

    pub async fn persist(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        tokio::fs::write(path, toml::to_string_pretty(self)?).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(path).await?.permissions();
            permissions.set_mode(0o600);
            tokio::fs::set_permissions(path, permissions).await?;
        }
        Ok(())
    }

    pub async fn validate(&self) -> anyhow::Result<()> {
        if self.interval_seconds == 0 {
            anyhow::bail!("interval_seconds must be greater than zero");
        }
        if !self.xray.api_endpoint.starts_with("http://127.0.0.1:")
            && !self.xray.api_endpoint.starts_with("http://[::1]:")
        {
            anyhow::bail!("Xray API must use a loopback HTTP endpoint");
        }
        if self.xray.inbound_tag.trim().is_empty() {
            anyhow::bail!("Xray inbound tag must not be empty");
        }
        if self.xray.listen_address.trim().is_empty()
            || self.xray.listen_port == 0
            || self.xray.protocol != "vless"
            || self.xray.transport != "tcp"
            || !matches!(self.xray.security.as_str(), "none" | "tls" | "reality")
        {
            anyhow::bail!("invalid Xray inbound settings");
        }
        if self.xray.security == "tls"
            && (self
                .xray
                .server_name
                .as_deref()
                .unwrap_or_default()
                .is_empty()
                || self
                    .xray
                    .tls_certificate_path
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
                || self
                    .xray
                    .tls_key_path
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty())
        {
            anyhow::bail!("TLS Xray settings require server_name and certificate/key paths");
        }
        if self.xray.security == "reality"
            && (self
                .xray
                .server_name
                .as_deref()
                .unwrap_or_default()
                .is_empty()
                || self
                    .xray
                    .reality_private_key
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
                || self.xray.reality_short_ids.is_empty())
        {
            anyhow::bail!("Reality Xray settings require server_name, private key, and short IDs");
        }
        if let Some(config) = self.xray.bootstrap_json.as_deref() {
            let value: serde_json::Value = serde_json::from_str(config)?;
            if !value.is_object() {
                anyhow::bail!("Xray bootstrap_json must be a JSON object");
            }
        }
        if self.spool.path.trim().is_empty()
            || self.spool.max_batches == 0
            || self.spool.max_bytes == 0
        {
            anyhow::bail!("traffic spool path and limits must be configured");
        }
        if self.tls.enabled {
            if !self.panel_endpoint.starts_with("https://") {
                anyhow::bail!("TLS-enabled agent endpoint must use https://");
            }
            if !Path::new(&self.tls.ca_path).is_file() {
                anyhow::bail!("TLS CA file does not exist: {}", self.tls.ca_path);
            }
            let identity_exists =
                Path::new(&self.tls.cert_path).is_file() && Path::new(&self.tls.key_path).is_file();
            if !identity_exists
                && (self.registration_token.trim().is_empty()
                    || !self.tls.enrollment_endpoint.starts_with("https://"))
            {
                anyhow::bail!(
                    "missing Agent certificate/key and no secure enrollment settings are available"
                );
            }
            if self.tls.domain_name.trim().is_empty() {
                anyhow::bail!("TLS domain_name must not be empty");
            }
            if self.tls.renew_before_days == 0 || self.tls.renew_before_days > 90 {
                anyhow::bail!("TLS renew_before_days must be between 1 and 90");
            }
        }
        Ok(())
    }
}
