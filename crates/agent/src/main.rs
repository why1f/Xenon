mod collector;
mod config;
mod control;
mod spool;
mod xray_api;
mod xray_config;

use anyhow::Context;
use config::AgentConfig;
use tracing::info;
use xenon_domain::{MAX_XRAY_CORE_VERSION, PANEL_AGENT_PROTOCOL_VERSION};
use xray_embedded_runner::{XraySupervisor, EMBEDDED_SHA256, EMBEDDED_VERSION};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    if std::env::args().nth(1).as_deref() == Some("version-info") {
        println!("agent_version={}", env!("CARGO_PKG_VERSION"));
        println!("protocol_version={PANEL_AGENT_PROTOCOL_VERSION}");
        println!("max_xray_version={MAX_XRAY_CORE_VERSION}");
        println!("embedded_xray_version={EMBEDDED_VERSION}");
        println!("embedded_xray_sha256={EMBEDDED_SHA256}");
        println!(
            "embedded_xray_available={}",
            XraySupervisor::embedded_available()
        );
        return Ok(());
    }
    let config_path = std::env::var("AGENT_CONFIG").unwrap_or_else(|_| "agent.toml".into());
    let config = AgentConfig::load(&config_path).await?;
    config.validate().await.context("validate agent config")?;
    info!(agent_id = %config.agent_id, node_id = %config.node_id, "agent starting");
    control::run(config, std::path::Path::new(&config_path))
        .await
        .context("agent control loop")
}
