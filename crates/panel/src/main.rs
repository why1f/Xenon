mod config;
mod grpc;
mod http;
mod maintenance;
mod secrets;
mod tui;

use anyhow::Context;
use chrono::Utc;
use config::PanelConfig;
use std::path::PathBuf;
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::info;

#[derive(Debug, Default)]
pub struct RuntimeState {
    pub connected_agents: usize,
    pub last_agent_event: Option<String>,
    pub agent_events: std::collections::VecDeque<String>,
}

impl RuntimeState {
    pub fn record_agent_event(&mut self, event: String) {
        self.last_agent_event = Some(event.clone());
        self.agent_events.push_back(event);
        while self.agent_events.len() > 200 {
            self.agent_events.pop_front();
        }
    }
}

enum Command {
    Run { headless: bool },
    Backup(PathBuf),
    Restore(PathBuf),
    Check(Option<PathBuf>),
}

fn parse_command() -> anyhow::Result<Command> {
    let mut args = std::env::args().skip(1);
    let Some(first) = args.next() else {
        return Ok(Command::Run { headless: false });
    };
    let command = match first.as_str() {
        "--headless" => Command::Run { headless: true },
        "backup" => Command::Backup(PathBuf::from(
            args.next()
                .context("usage: xenon backup <destination.db>")?,
        )),
        "restore" => Command::Restore(PathBuf::from(
            args.next().context("usage: xenon restore <backup.db>")?,
        )),
        "check-db" | "check" => Command::Check(args.next().map(PathBuf::from)),
        "--help" | "-h" => {
            println!(
                "xenon [--headless]\n\
                 xenon backup <destination.db>\n\
                 xenon restore <backup.db>\n\
                 xenon check-db [database.db]"
            );
            std::process::exit(0);
        }
        other => anyhow::bail!("unknown command: {other}; use --help for usage"),
    };
    if args.next().is_some() {
        anyhow::bail!("too many command arguments; use --help for usage");
    }
    Ok(command)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = parse_command()?;
    let env_headless = std::env::var("PANEL_HEADLESS").is_ok_and(|value| value == "1");
    let interactive_tui = matches!(&command, Command::Run { headless: false }) && !env_headless;
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()));
    if interactive_tui {
        // Writing tracing output to stdout/stderr corrupts ratatui's alternate screen.
        subscriber.with_writer(std::io::sink).init();
    } else {
        subscriber.init();
    }

    let config_path = std::env::var("XENON_CONFIG").unwrap_or_else(|_| "xenon.toml".into());
    let config = PanelConfig::load(&config_path).await?;
    match command {
        Command::Backup(destination) => {
            let database = xenon_storage::Database::connect(&config.database_path)
                .await
                .context("connect panel database for backup")?;
            let outcome = maintenance::create_backup(&database, &destination).await?;
            database.close().await;
            println!(
                "backup created: {}\nsha256={}\nschema_version={}",
                outcome.path.display(),
                outcome.checksum,
                outcome.verification.schema_version
            );
            return Ok(());
        }
        Command::Check(path) => {
            let path = path.unwrap_or_else(|| PathBuf::from(&config.database_path));
            let verification = maintenance::check_database(&path).await?;
            println!(
                "database ok: {}\nschema_version={} migrations={} foreign_key_violations={}",
                path.display(),
                verification.schema_version,
                verification.applied_migrations,
                verification.foreign_key_violations
            );
            return Ok(());
        }
        Command::Restore(backup) => {
            let outcome = maintenance::restore_database(&config.database_path, &backup).await?;
            println!(
                "database restored: {}\nsha256={}\nschema_version={}\nrollback={}",
                outcome.path.display(),
                outcome.checksum,
                outcome.verification.schema_version,
                outcome
                    .rollback_path
                    .as_deref()
                    .map_or_else(|| "none".to_string(), |path| path.display().to_string())
            );
            return Ok(());
        }
        Command::Run { headless } => {
            config.validate().await.context("validate panel config")?;
            let _database_lock = maintenance::acquire_database_lock(&config.database_path)?;
            run_panel(config, headless).await
        }
    }
}

async fn run_panel(config: PanelConfig, headless: bool) -> anyhow::Result<()> {
    let agent_ca_pem = if config.agent_install.enabled && !config.agent_install.ca_path.is_empty() {
        Some(
            tokio::fs::read(&config.agent_install.ca_path)
                .await
                .context("read public Agent trust CA")?,
        )
    } else {
        None
    };
    let agent_bootstrap = (config.agent_install.enabled
        && !config.agent_install.bootstrap_url.is_empty())
    .then(|| config.agent_install.bootstrap_manifest().into_bytes());
    let database = xenon_storage::Database::connect(&config.database_path)
        .await
        .context("connect panel database")?;
    database.ping().await.context("ping panel database")?;
    database
        .ensure_default_admin(Utc::now().timestamp())
        .await
        .context("create default admin")?;
    database
        .reset_stale_presence(Utc::now().timestamp())
        .await
        .context("reset stale agent presence")?;

    let state = Arc::new(RwLock::new(RuntimeState::default()));
    let grpc_state = state.clone();
    let http_state = state.clone();
    let grpc_addr = config.grpc_addr.clone();
    let grpc_tls = config.tls.clone();
    let grpc_registration = config.registration.clone();
    let grpc_enrollment = config.enrollment.clone();
    let grpc_database = database.clone();
    let enrollment_enabled = config.enrollment.enabled;
    let enrollment_tls = config.tls.clone();
    let enrollment_config = config.enrollment.clone();
    let enrollment_database = database.clone();
    let http_database = database.clone();
    let http_addr = config.http_addr.clone();
    let subscription_http = config.subscription_http.clone();
    let subscription_base_url = subscription_http.public_base_url(&http_addr);
    let traffic_database = database.clone();
    let traffic_retention = config.traffic_retention.clone();
    let backup_database = database.clone();
    let backup_config = config.backup.clone();
    let (server_failure_tx, mut server_failure_rx) =
        tokio::sync::mpsc::unbounded_channel::<anyhow::Error>();

    let grpc_failure_tx = server_failure_tx.clone();
    let grpc_task = tokio::spawn(async move {
        let result = grpc::serve(
            grpc_addr,
            grpc_state,
            grpc_database,
            grpc_tls,
            grpc_registration,
            grpc_enrollment,
        )
        .await;
        let error = result
            .context("panel gRPC server stopped")
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("panel gRPC server stopped unexpectedly"));
        tracing::error!(%error, "critical Panel server stopped");
        let _ = grpc_failure_tx.send(error);
    });
    let enrollment_task = if enrollment_enabled {
        let enrollment_failure_tx = server_failure_tx.clone();
        Some(tokio::spawn(async move {
            let result =
                grpc::serve_enrollment(enrollment_tls, enrollment_config, enrollment_database)
                    .await;
            let error = result
                .context("panel enrollment server stopped")
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("panel enrollment server stopped unexpectedly"));
            tracing::error!(%error, "critical Panel server stopped");
            let _ = enrollment_failure_tx.send(error);
        }))
    } else {
        None
    };
    let http_failure_tx = server_failure_tx.clone();
    let http_task = tokio::spawn(async move {
        let result = http::serve(
            http_addr,
            subscription_http,
            http_state,
            http_database,
            agent_ca_pem,
            agent_bootstrap,
        )
        .await;
        let error = result
            .context("subscription HTTP server stopped")
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("subscription HTTP server stopped unexpectedly"));
        tracing::error!(%error, "critical Panel server stopped");
        let _ = http_failure_tx.send(error);
    });
    drop(server_failure_tx);
    let traffic_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(
            traffic_retention.maintenance_interval_seconds,
        ));
        loop {
            interval.tick().await;
            match traffic_database
                .prune_traffic_history(
                    Utc::now().timestamp(),
                    traffic_retention.raw_event_days,
                    traffic_retention.interface_snapshot_days,
                    traffic_retention.system_snapshot_days,
                    traffic_retention.hourly_aggregate_days,
                    traffic_retention.daily_aggregate_days,
                )
                .await
            {
                Ok(result) => tracing::info!(
                    xray_events = result.xray_events,
                    interface_snapshots = result.interface_snapshots,
                    system_snapshots = result.system_snapshots,
                    hourly_aggregates = result.hourly_aggregates,
                    daily_aggregates = result.daily_aggregates,
                    "traffic retention completed"
                ),
                Err(error) => tracing::error!(%error, "traffic retention failed"),
            }
        }
    });
    let backup_task = if backup_config.enabled {
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(
                backup_config.interval_hours.saturating_mul(3_600),
            ));
            loop {
                interval.tick().await;
                match maintenance::create_scheduled_backup(
                    &backup_database,
                    &backup_config.directory,
                    backup_config.retain_count,
                )
                .await
                {
                    Ok(outcome) => tracing::info!(
                        path = %outcome.path.display(),
                        sha256 = outcome.checksum,
                        schema_version = outcome.verification.schema_version,
                        "scheduled database backup completed"
                    ),
                    Err(error) => tracing::error!(%error, "scheduled database backup failed"),
                }
            }
        }))
    } else {
        None
    };

    let headless = headless || std::env::var("PANEL_HEADLESS").is_ok_and(|value| value == "1");
    let interaction = async {
        if headless {
            info!("panel started in headless mode");
            tokio::signal::ctrl_c().await.context("wait for Ctrl+C")
        } else {
            info!("panel started; press q to quit");
            tui::run(
                state,
                database.clone(),
                config.grpc_addr.clone(),
                subscription_base_url,
                config.agent_install.clone(),
            )
            .await
        }
    };
    tokio::pin!(interaction);
    let run_result = tokio::select! {
        result = &mut interaction => result,
        error = server_failure_rx.recv() => Err(error.unwrap_or_else(|| {
            anyhow::anyhow!("all Panel network server tasks stopped unexpectedly")
        })),
    };
    grpc_task.abort();
    if let Some(task) = enrollment_task {
        task.abort();
    }
    http_task.abort();
    traffic_task.abort();
    if let Some(task) = backup_task {
        task.abort();
    }
    run_result
}
