//! SQLite connection and migrations for Panel.

pub mod models;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqliteConnection, SqlitePool,
};
use std::{collections::HashSet, path::Path};
use thiserror::Error;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("invalid sqlite path: {0}")]
    InvalidPath(String),
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("validation error: {0}")]
    Validation(String),
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| StorageError::InvalidPath(error.to_string()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn latest_schema_version() -> i64 {
        MIGRATOR
            .migrations
            .last()
            .map_or(0, |migration| migration.version)
    }

    pub async fn close(self) {
        self.pool.close().await;
    }

    pub async fn backup_to(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<models::DatabaseVerification, StorageError> {
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(StorageError::Validation(format!(
                "backup destination already exists: {}",
                destination.display()
            )));
        }
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| StorageError::InvalidPath(error.to_string()))?;
        }
        let destination = destination.to_string_lossy().into_owned();
        sqlx::query("VACUUM INTO ?")
            .bind(&destination)
            .execute(&self.pool)
            .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|error| StorageError::InvalidPath(error.to_string()))?;
        }
        Self::verify_file(&destination).await
    }

    pub async fn verify_file(
        path: impl AsRef<Path>,
    ) -> Result<models::DatabaseVerification, StorageError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(StorageError::InvalidPath(format!(
                "database file does not exist: {}",
                path.display()
            )));
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let result = Self::verify_pool(&pool).await;
        pool.close().await;
        result
    }

    async fn verify_pool(pool: &SqlitePool) -> Result<models::DatabaseVerification, StorageError> {
        let integrity_messages = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_all(pool)
            .await?;
        if integrity_messages.as_slice() != ["ok"] {
            return Err(StorageError::Validation(format!(
                "SQLite integrity check failed: {}",
                integrity_messages.join("; ")
            )));
        }
        let foreign_key_violations =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(pool)
                .await?;
        if foreign_key_violations != 0 {
            return Err(StorageError::Validation(format!(
                "database has {foreign_key_violations} foreign key violation(s)"
            )));
        }
        let applied = sqlx::query_as::<_, (i64, Vec<u8>, i64)>(
            "SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version ASC",
        )
        .fetch_all(pool)
        .await?;
        for (version, checksum, success) in &applied {
            if *success != 1 {
                return Err(StorageError::Validation(format!(
                    "database contains failed migration {version}"
                )));
            }
            let Some(expected) = MIGRATOR
                .migrations
                .iter()
                .find(|migration| migration.version == *version)
            else {
                return Err(StorageError::Validation(format!(
                    "database schema version {version} is unknown to this Panel binary"
                )));
            };
            if checksum.as_slice() != expected.checksum.as_ref() {
                return Err(StorageError::Validation(format!(
                    "migration checksum mismatch at version {version}"
                )));
            }
        }
        let schema_version = applied.last().map_or(0, |(version, _, _)| *version);
        Ok(models::DatabaseVerification {
            schema_version,
            integrity_messages,
            foreign_key_violations,
            applied_migrations: applied.len() as i64,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn ensure_default_admin(&self, now: i64) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT OR IGNORE INTO users
             (id, username, display_name, status, created_at, updated_at)
             VALUES (?, 'admin', 'Administrator', 'active', ?, ?)",
        )
        .bind("00000000-0000-7000-8000-000000000001")
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_registration_token(
        &self,
        token: &models::NewRegistrationToken,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO registration_tokens
                (id, node_id, token_hash, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&token.id)
        .bind(&token.node_id)
        .bind(&token.token_hash)
        .bind(token.expires_at)
        .bind(token.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn registration_token_can_enroll(
        &self,
        token_hash: &str,
        agent_id: &str,
        node_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let available = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM registration_tokens
                WHERE token_hash = ? AND node_id = ? AND expires_at >= ?
                  AND (consumed_at IS NULL OR consumed_agent_id = ?)
             )",
        )
        .bind(token_hash)
        .bind(node_id)
        .bind(now)
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(available == 1)
    }

    pub async fn create_node_with_registration(
        &self,
        node: &models::NewNode,
        registration: &models::NewRegistrationToken,
    ) -> Result<(), StorageError> {
        if node.name.trim().is_empty()
            || node.landing_host.trim().is_empty()
            || node.xray_listen_port <= 0
            || node.xray_listen_port > 65_535
            || registration.node_id != node.id
            || registration.expires_at <= registration.created_at
        {
            return Err(StorageError::Validation("invalid node settings".into()));
        }
        if node.protocol != "vless"
            || node.transport != "tcp"
            || !matches!(node.security.as_str(), "none" | "tls" | "reality")
        {
            return Err(StorageError::Validation(
                "only VLESS TCP with none, TLS, or Reality security is supported".into(),
            ));
        }
        if matches!(node.security.as_str(), "tls" | "reality")
            && missing_text(node.server_name.as_deref())
        {
            return Err(StorageError::Validation(
                "TLS and Reality nodes require server_name".into(),
            ));
        }
        if node.security == "reality"
            && (missing_text(node.reality_public_key.as_deref())
                || missing_text(node.reality_short_id.as_deref()))
        {
            return Err(StorageError::Validation(
                "Reality nodes require public key and short ID".into(),
            ));
        }
        if node.landing_host.contains("://") || node.landing_host.contains('/') {
            return Err(StorageError::Validation(
                "landing host must be a host or IP, not a URL".into(),
            ));
        }
        if node.publish_host.is_some() != node.publish_port.is_some()
            || node
                .publish_port
                .is_some_and(|port| !(1..=65_535).contains(&port))
        {
            return Err(StorageError::Validation(
                "publish host and port must be provided together".into(),
            ));
        }
        if node
            .publish_host
            .as_deref()
            .is_some_and(|host| host.contains("://") || host.contains('/'))
        {
            return Err(StorageError::Validation(
                "publish host must be a host or IP, not a URL".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO nodes
                (id, name, landing_host, xray_listen_port, publish_host,
                 publish_port, protocol, transport, security, server_name,
                 reality_public_key, reality_short_id, reality_fingerprint,
                 status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&node.id)
        .bind(&node.name)
        .bind(&node.landing_host)
        .bind(node.xray_listen_port)
        .bind(&node.publish_host)
        .bind(node.publish_port)
        .bind(&node.protocol)
        .bind(&node.transport)
        .bind(&node.security)
        .bind(&node.server_name)
        .bind(&node.reality_public_key)
        .bind(&node.reality_short_id)
        .bind(&node.reality_fingerprint)
        .bind(node.created_at)
        .bind(node.created_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO registration_tokens
                (id, node_id, token_hash, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&registration.id)
        .bind(&registration.node_id)
        .bind(&registration.token_hash)
        .bind(registration.expires_at)
        .bind(registration.created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_user_subscription(
        &self,
        input: &models::NewSubscription,
    ) -> Result<(), StorageError> {
        if input.node_ids.is_empty() {
            return Err(StorageError::Validation(
                "subscription must contain at least one node".into(),
            ));
        }
        for binding in &input.nic_bindings {
            if !input.node_ids.contains(&binding.node_id) {
                return Err(StorageError::Validation(format!(
                    "NIC binding node is not selected: {}",
                    binding.node_id
                )));
            }
            if binding.interface_name.trim().is_empty()
                || binding.traffic_limit_bytes <= 0
                || binding.initial_used_bytes < 0
                || !matches!(
                    binding.billing_direction.as_str(),
                    "rx_tx" | "tx_only" | "rx_only"
                )
            {
                return Err(StorageError::Validation(
                    "invalid NIC binding settings".into(),
                ));
            }
            xenon_domain::ResetPolicy::from_stored(&binding.reset_policy, binding.reset_anchor)
                .map_err(|error| StorageError::Validation(error.to_string()))?;
            if binding
                .current_cycle_end
                .is_some_and(|end| end <= binding.current_cycle_start)
            {
                return Err(StorageError::Validation("invalid NIC billing cycle".into()));
            }
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO users
                (id, username, status, created_at, updated_at)
             VALUES (?, ?, 'active', ?, ?)",
        )
        .bind(&input.user_id)
        .bind(&input.username)
        .bind(input.created_at)
        .bind(input.created_at)
        .execute(&mut *tx)
        .await?;

        let actual_user_id =
            sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE username = ?")
                .bind(&input.username)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query(
            "INSERT INTO subscriptions
                (id, user_id, name, token_hash, xray_uuid, xray_email, status,
                 starts_at, expires_at, traffic_limit_bytes,
                traffic_multiplier_basis_points, reset_policy, reset_anchor,
                 current_cycle_start, current_cycle_end, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 'active', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.subscription_id)
        .bind(actual_user_id)
        .bind(&input.name)
        .bind(&input.token_hash)
        .bind(&input.xray_uuid)
        .bind(&input.xray_email)
        .bind(input.starts_at)
        .bind(input.expires_at)
        .bind(input.traffic_limit_bytes)
        .bind(input.traffic_multiplier_basis_points)
        .bind(&input.reset_policy)
        .bind(input.reset_anchor)
        .bind(input.current_cycle_start)
        .bind(input.current_cycle_end)
        .bind(input.created_at)
        .bind(input.created_at)
        .execute(&mut *tx)
        .await?;

        for (sort_order, node_id) in input.node_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO subscription_nodes
                    (subscription_id, node_id, enabled, sort_order)
                 VALUES (?, ?, 1, ?)",
            )
            .bind(&input.subscription_id)
            .bind(node_id)
            .bind(sort_order as i64)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE nodes
                 SET desired_revision = desired_revision + 1, updated_at = ?
                 WHERE id = ?",
            )
            .bind(input.created_at)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        }
        for binding in &input.nic_bindings {
            let interface_exists = sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(
                    SELECT 1 FROM interface_snapshots
                    WHERE node_id = ? AND interface_name = ?
                )",
            )
            .bind(&binding.node_id)
            .bind(&binding.interface_name)
            .fetch_one(&mut *tx)
            .await?;
            if interface_exists != 1 {
                return Err(StorageError::Validation(format!(
                    "interface has not been reported: {}/{}",
                    binding.node_id, binding.interface_name
                )));
            }
            sqlx::query(
                "INSERT INTO nic_bindings
                    (id, subscription_id, node_id, interface_name,
                     billing_direction, traffic_limit_bytes, initial_used_bytes,
                     reset_policy, reset_anchor, bound_at, enabled,
                     current_cycle_start, current_cycle_end)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
            )
            .bind(&binding.id)
            .bind(&input.subscription_id)
            .bind(&binding.node_id)
            .bind(&binding.interface_name)
            .bind(&binding.billing_direction)
            .bind(binding.traffic_limit_bytes)
            .bind(binding.initial_used_bytes)
            .bind(&binding.reset_policy)
            .bind(binding.reset_anchor)
            .bind(input.created_at)
            .bind(binding.current_cycle_start)
            .bind(binding.current_cycle_end)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_subscription(
        &self,
        subscription_id: &str,
        input: &models::UpdateSubscription,
    ) -> Result<bool, StorageError> {
        if input.name.trim().is_empty()
            || !matches!(input.status.as_str(), "active" | "disabled")
            || input.traffic_limit_bytes.is_some_and(|value| value <= 0)
            || input.traffic_multiplier_basis_points <= 0
            || input.node_ids.is_empty()
        {
            return Err(StorageError::Validation(
                "invalid subscription settings".into(),
            ));
        }
        let unique_nodes = input.node_ids.iter().collect::<HashSet<_>>();
        if unique_nodes.len() != input.node_ids.len() {
            return Err(StorageError::Validation(
                "subscription node IDs must be unique".into(),
            ));
        }
        xenon_domain::ResetPolicy::from_stored(&input.reset_policy, input.reset_anchor)
            .map_err(|error| StorageError::Validation(error.to_string()))?;

        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query_as::<_, (i64, i64)>(
            "SELECT starts_at, current_cycle_start FROM subscriptions WHERE id = ?",
        )
        .bind(subscription_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((starts_at, current_cycle_start)) = existing else {
            tx.rollback().await?;
            return Ok(false);
        };
        if input.expires_at.is_some_and(|expires| expires <= starts_at)
            || input
                .current_cycle_end
                .is_some_and(|end| end <= current_cycle_start)
        {
            tx.rollback().await?;
            return Err(StorageError::Validation(
                "invalid subscription period".into(),
            ));
        }
        for node_id in &input.node_ids {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(
                    SELECT 1 FROM nodes
                    WHERE id = ? AND management_status = 'active'
                )",
            )
            .bind(node_id)
            .fetch_one(&mut *tx)
            .await?;
            if exists != 1 {
                tx.rollback().await?;
                return Err(StorageError::Validation(format!(
                    "unknown or disabled node: {node_id}"
                )));
            }
        }
        let old_nodes = sqlx::query_scalar::<_, String>(
            "SELECT node_id FROM subscription_nodes
             WHERE subscription_id = ? AND enabled = 1",
        )
        .bind(subscription_id)
        .fetch_all(&mut *tx)
        .await?;
        for node_id in old_nodes
            .iter()
            .filter(|node_id| !unique_nodes.contains(node_id))
        {
            let has_binding = sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(
                    SELECT 1 FROM nic_bindings
                    WHERE subscription_id = ? AND node_id = ?
                      AND enabled = 1 AND unbound_at IS NULL
                )",
            )
            .bind(subscription_id)
            .bind(node_id)
            .fetch_one(&mut *tx)
            .await?;
            if has_binding == 1 {
                tx.rollback().await?;
                return Err(StorageError::Validation(format!(
                    "unbind NICs before removing node: {node_id}"
                )));
            }
        }
        sqlx::query(
            "UPDATE subscriptions
             SET name = ?, status = ?, expires_at = ?, traffic_limit_bytes = ?,
                 traffic_multiplier_basis_points = ?, reset_policy = ?,
                 reset_anchor = ?, current_cycle_end = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(input.name.trim())
        .bind(&input.status)
        .bind(input.expires_at)
        .bind(input.traffic_limit_bytes)
        .bind(input.traffic_multiplier_basis_points)
        .bind(&input.reset_policy)
        .bind(input.reset_anchor)
        .bind(input.current_cycle_end)
        .bind(input.updated_at)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE subscription_nodes SET enabled = 0 WHERE subscription_id = ?")
            .bind(subscription_id)
            .execute(&mut *tx)
            .await?;
        for (sort_order, node_id) in input.node_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO subscription_nodes
                    (subscription_id, node_id, enabled, sort_order)
                 VALUES (?, ?, 1, ?)
                 ON CONFLICT(subscription_id, node_id) DO UPDATE SET
                    enabled = 1, sort_order = excluded.sort_order",
            )
            .bind(subscription_id)
            .bind(node_id)
            .bind(sort_order as i64)
            .execute(&mut *tx)
            .await?;
        }
        let affected_nodes = old_nodes
            .into_iter()
            .chain(input.node_ids.iter().cloned())
            .collect::<HashSet<_>>();
        for node_id in affected_nodes {
            sqlx::query(
                "UPDATE nodes
                 SET desired_revision = desired_revision + 1, updated_at = ?
                 WHERE id = ?",
            )
            .bind(input.updated_at)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn rotate_subscription_token(
        &self,
        subscription_id: &str,
        token_hash: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        if token_hash.len() != 64 || !token_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StorageError::Validation(
                "invalid subscription token hash".into(),
            ));
        }
        let result = sqlx::query(
            "UPDATE subscriptions SET token_hash = ?, updated_at = ?
             WHERE id = ? AND status = 'active'",
        )
        .bind(token_hash.to_ascii_lowercase())
        .bind(now)
        .bind(subscription_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn rotate_subscription_uuid(
        &self,
        subscription_id: &str,
        xray_uuid: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        uuid::Uuid::parse_str(xray_uuid)
            .map_err(|_| StorageError::Validation("invalid subscription UUID".into()))?;
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE subscriptions SET xray_uuid = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(xray_uuid)
        .bind(now)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE nodes SET desired_revision = desired_revision + 1, updated_at = ?
             WHERE id IN (
                 SELECT node_id FROM subscription_nodes
                 WHERE subscription_id = ? AND enabled = 1
             )",
        )
        .bind(now)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn advance_billing_cycles(&self, now: i64) -> Result<u64, StorageError> {
        let subscriptions = sqlx::query_as::<_, (String, String, Option<i64>, i64, i64)>(
            "SELECT id, reset_policy, reset_anchor, starts_at, current_cycle_start
             FROM subscriptions
             WHERE current_cycle_end IS NOT NULL AND current_cycle_end <= ?",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        let bindings = sqlx::query_as::<_, (String, String, Option<i64>, i64, i64)>(
            "SELECT id, reset_policy, reset_anchor, bound_at, current_cycle_start
             FROM nic_bindings
             WHERE enabled = 1 AND unbound_at IS NULL
               AND current_cycle_end IS NOT NULL AND current_cycle_end <= ?",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        let mut tx = self.pool.begin().await?;
        let mut advanced = 0_u64;
        for (id, policy, anchor, effective_start, previous_start) in subscriptions {
            let policy = xenon_domain::ResetPolicy::from_stored(&policy, anchor)
                .map_err(|error| StorageError::Validation(error.to_string()))?;
            let cycle = policy
                .cycle_at(effective_start, now)
                .map_err(|error| StorageError::Validation(error.to_string()))?;
            if cycle.start <= previous_start {
                continue;
            }
            let updated = sqlx::query(
                "UPDATE subscriptions
                 SET current_cycle_start = ?, current_cycle_end = ?, updated_at = ?
                 WHERE id = ? AND current_cycle_start < ?",
            )
            .bind(cycle.start)
            .bind(cycle.end)
            .bind(now)
            .bind(&id)
            .bind(cycle.start)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 1 {
                sqlx::query(
                    "UPDATE nodes
                     SET desired_revision = desired_revision + 1, updated_at = ?
                     WHERE id IN (
                         SELECT node_id FROM subscription_nodes
                         WHERE subscription_id = ? AND enabled = 1
                     )",
                )
                .bind(now)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
                advanced += 1;
            }
        }
        for (id, policy, anchor, effective_start, previous_start) in bindings {
            let policy = xenon_domain::ResetPolicy::from_stored(&policy, anchor)
                .map_err(|error| StorageError::Validation(error.to_string()))?;
            let cycle = policy
                .cycle_at(effective_start, now)
                .map_err(|error| StorageError::Validation(error.to_string()))?;
            if cycle.start <= previous_start {
                continue;
            }
            let updated = sqlx::query(
                "UPDATE nic_bindings
                 SET current_cycle_start = ?, current_cycle_end = ?
                 WHERE id = ? AND current_cycle_start < ?",
            )
            .bind(cycle.start)
            .bind(cycle.end)
            .bind(&id)
            .bind(cycle.start)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 1 {
                Self::rebuild_nic_binding_cycle(&mut tx, &id).await?;
                advanced += 1;
            }
        }
        tx.commit().await?;
        Ok(advanced)
    }

    pub async fn reset_subscription_cycle(
        &self,
        subscription_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let record = sqlx::query_as::<_, (String, Option<i64>, i64)>(
            "SELECT reset_policy, reset_anchor, starts_at
             FROM subscriptions WHERE id = ?",
        )
        .bind(subscription_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((policy, anchor, starts_at)) = record else {
            tx.rollback().await?;
            return Ok(false);
        };
        let policy = xenon_domain::ResetPolicy::from_stored(&policy, anchor)
            .map_err(|error| StorageError::Validation(error.to_string()))?;
        let next_end = policy
            .cycle_at(starts_at, now)
            .map_err(|error| StorageError::Validation(error.to_string()))?
            .end;
        sqlx::query(
            "UPDATE subscriptions
             SET current_cycle_start = ?, current_cycle_end = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(next_end)
        .bind(now)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE nodes
             SET desired_revision = desired_revision + 1, updated_at = ?
             WHERE id IN (
                 SELECT node_id FROM subscription_nodes
                 WHERE subscription_id = ? AND enabled = 1
             )",
        )
        .bind(now)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn list_user_summaries(
        &self,
        now: i64,
    ) -> Result<Vec<models::UserSummary>, StorageError> {
        let rows = sqlx::query_as::<_, models::UserSummary>(
            "SELECT
                u.id,
                u.username,
                u.display_name,
                u.status,
                COUNT(usage.subscription_id) AS subscription_count,
                COALESCE(SUM(usage.charged_bytes), 0) AS charged_bytes,
                CASE
                    WHEN SUM(CASE
                        WHEN usage.subscription_id IS NOT NULL
                             AND usage.traffic_limit_bytes IS NULL THEN 1
                        ELSE 0
                    END) > 0 THEN NULL
                    ELSE SUM(usage.traffic_limit_bytes)
                END AS traffic_limit_bytes,
                COALESCE(SUM(usage.expired), 0) AS expired_subscriptions
             FROM users u
             LEFT JOIN (
                 SELECT
                     s.id AS subscription_id,
                     s.user_id,
                     s.traffic_limit_bytes,
                     CASE
                         WHEN s.expires_at IS NOT NULL AND s.expires_at <= ? THEN 1
                         ELSE 0
                     END AS expired,
                     COALESCE(SUM(a.uplink_bytes + a.downlink_bytes), 0)
                         * s.traffic_multiplier_basis_points / 10000 AS charged_bytes
                 FROM subscriptions s
                 LEFT JOIN xray_traffic_aggregates a
                   ON a.subscription_id = s.id
                  AND a.granularity = 'cycle'
                  AND a.bucket_start = s.current_cycle_start
                 GROUP BY s.id, s.user_id, s.traffic_multiplier_basis_points,
                          s.traffic_limit_bytes, s.expires_at
             ) usage ON usage.user_id = u.id
             GROUP BY u.id, u.username, u.display_name, u.status
             ORDER BY charged_bytes DESC, u.username ASC",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn latest_nic_totals(
        &self,
    ) -> Result<Option<models::NicCounterTotals>, StorageError> {
        let totals = sqlx::query_as::<_, models::NicCounterTotals>(
            "WITH ranked AS (
                SELECT rx_absolute, tx_absolute, sampled_at,
                       ROW_NUMBER() OVER (
                           PARTITION BY node_id, interface_name
                           ORDER BY sampled_at DESC, sequence DESC
                       ) AS rn
                FROM interface_snapshots
             )
             SELECT COALESCE(SUM(rx_absolute), 0) AS rx_bytes,
                    COALESCE(SUM(tx_absolute), 0) AS tx_bytes,
                    COALESCE(MAX(sampled_at), 0) AS sampled_at
             FROM ranked
             WHERE rn = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        if totals.sampled_at == 0 {
            Ok(None)
        } else {
            Ok(Some(totals))
        }
    }

    pub async fn list_nodes(&self) -> Result<Vec<models::NodeRecord>, StorageError> {
        let rows = sqlx::query_as::<_, models::NodeRecord>(
            "SELECT id, name, landing_host, xray_listen_port,
                    publish_host, publish_port, protocol, transport, security,
                    server_name, reality_public_key, reality_short_id,
                    reality_fingerprint
             FROM nodes WHERE management_status != 'deleted' ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn update_node(
        &self,
        node_id: &str,
        node: &models::UpdateNode,
    ) -> Result<bool, StorageError> {
        if node.name.trim().is_empty()
            || node.landing_host.trim().is_empty()
            || !(1..=65_535).contains(&node.xray_listen_port)
            || !matches!(node.security.as_str(), "none" | "tls" | "reality")
        {
            return Err(StorageError::Validation("invalid node settings".into()));
        }
        if matches!(node.security.as_str(), "tls" | "reality")
            && missing_text(node.server_name.as_deref())
        {
            return Err(StorageError::Validation(
                "TLS and Reality nodes require server_name".into(),
            ));
        }
        if node.security == "reality"
            && (missing_text(node.reality_public_key.as_deref())
                || missing_text(node.reality_short_id.as_deref()))
        {
            return Err(StorageError::Validation(
                "Reality nodes require public key and short ID".into(),
            ));
        }
        if node.landing_host.contains("://") || node.landing_host.contains('/') {
            return Err(StorageError::Validation(
                "landing host must be a host or IP, not a URL".into(),
            ));
        }
        if node.publish_host.is_some() != node.publish_port.is_some()
            || node
                .publish_port
                .is_some_and(|port| !(1..=65_535).contains(&port))
            || node
                .publish_host
                .as_deref()
                .is_some_and(|host| host.contains("://") || host.contains('/'))
        {
            return Err(StorageError::Validation(
                "invalid publish host or port".into(),
            ));
        }
        let result = sqlx::query(
            "UPDATE nodes
             SET name = ?, landing_host = ?, xray_listen_port = ?,
                 publish_host = ?, publish_port = ?, security = ?,
                 server_name = ?, reality_public_key = ?, reality_short_id = ?,
                 reality_fingerprint = ?, desired_revision = desired_revision + 1,
                 updated_at = ?
             WHERE id = ? AND management_status != 'deleted'",
        )
        .bind(node.name.trim())
        .bind(node.landing_host.trim())
        .bind(node.xray_listen_port)
        .bind(&node.publish_host)
        .bind(node.publish_port)
        .bind(&node.security)
        .bind(&node.server_name)
        .bind(&node.reality_public_key)
        .bind(&node.reality_short_id)
        .bind(&node.reality_fingerprint)
        .bind(node.updated_at)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn set_node_management_status(
        &self,
        node_id: &str,
        status: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        if !matches!(status, "active" | "disabled") {
            return Err(StorageError::Validation(
                "invalid node management status".into(),
            ));
        }
        let result = sqlx::query(
            "UPDATE nodes
             SET management_status = ?, desired_revision = desired_revision + 1,
                 updated_at = ?
             WHERE id = ? AND management_status != 'deleted'",
        )
        .bind(status)
        .bind(now)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_node(&self, node_id: &str, now: i64) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM nodes WHERE id = ? AND management_status != 'deleted'
            )",
        )
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if exists != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        let references = sqlx::query_as::<_, (i64, i64)>(
            "SELECT
                (SELECT COUNT(*) FROM subscription_nodes
                 WHERE node_id = ? AND enabled = 1),
                (SELECT COUNT(*) FROM nic_bindings
                 WHERE node_id = ? AND enabled = 1 AND unbound_at IS NULL)",
        )
        .bind(node_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if references.0 > 0 || references.1 > 0 {
            tx.rollback().await?;
            return Err(StorageError::Validation(
                "remove subscriptions and NIC bindings before deleting node".into(),
            ));
        }
        sqlx::query("UPDATE nodes SET management_status = 'deleted', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE agent_certificates SET revoked_at = ?
             WHERE node_id = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE agents SET status = 'revoked', updated_at = ? WHERE node_id = ?")
            .bind(now)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE registration_tokens SET expires_at = MIN(expires_at, ?)
             WHERE node_id = ? AND consumed_at IS NULL",
        )
        .bind(now)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn list_node_overviews(&self) -> Result<Vec<models::NodeOverview>, StorageError> {
        let rows = sqlx::query_as::<_, models::NodeOverview>(
            "SELECT n.id, n.name, n.landing_host, n.xray_listen_port,
                    n.publish_host, n.publish_port, n.protocol, n.transport,
                    n.security, n.server_name, n.reality_public_key,
                    n.reality_short_id, n.reality_fingerprint,
                    n.status AS node_status, n.management_status,
                    n.desired_revision, a.status AS agent_status,
                    a.last_seen_at, a.agent_version, a.xray_version,
                    sys.cpu_usage_basis_points, sys.load_1_milli,
                    sys.memory_total_bytes, sys.memory_used_bytes,
                    sys.disk_total_bytes, sys.disk_used_bytes
             FROM nodes n
             LEFT JOIN agents a ON a.node_id = n.id
             LEFT JOIN system_snapshots sys ON sys.rowid = (
                 SELECT snapshot.rowid
                 FROM system_snapshots snapshot
                 WHERE snapshot.node_id = n.id
                 ORDER BY snapshot.sampled_at DESC, snapshot.sequence DESC
                 LIMIT 1
             )
             WHERE n.management_status != 'deleted'
             ORDER BY n.name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn list_recent_interfaces(
        &self,
    ) -> Result<Vec<models::InterfaceRecord>, StorageError> {
        let rows = sqlx::query_as::<_, models::InterfaceRecord>(
            "SELECT node_id, interface_name, MAX(sampled_at) AS sampled_at
             FROM interface_snapshots
             GROUP BY node_id, interface_name
             ORDER BY node_id, interface_name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn desired_xray_users_for_node(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<(u64, Vec<models::DesiredXrayUser>), StorageError> {
        self.advance_billing_cycles(now).await?;
        let node = sqlx::query_as::<_, (i64, String)>(
            "SELECT desired_revision, management_status FROM nodes WHERE id = ?",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StorageError::Validation(format!("unknown node: {node_id}")))?;
        let (revision, management_status) = node;
        if management_status != "active" {
            return Ok((revision.max(0) as u64, Vec::new()));
        }
        let users = sqlx::query_as::<_, models::DesiredXrayUser>(
            "SELECT s.id AS subscription_id, s.xray_email, s.xray_uuid
             FROM subscriptions s
             INNER JOIN subscription_nodes sn ON sn.subscription_id = s.id
             WHERE sn.node_id = ? AND sn.enabled = 1
               AND s.status = 'active'
               AND s.starts_at <= ?
               AND (s.expires_at IS NULL OR s.expires_at > ?)
               AND (
                    s.traffic_limit_bytes IS NULL OR
                    COALESCE((
                        SELECT SUM(a.uplink_bytes + a.downlink_bytes)
                               * s.traffic_multiplier_basis_points / 10000
                        FROM xray_traffic_aggregates a
                        WHERE a.subscription_id = s.id
                          AND a.granularity = 'cycle'
                          AND a.bucket_start = s.current_cycle_start
                    ), 0) < s.traffic_limit_bytes
               )
             ORDER BY s.xray_email",
        )
        .bind(node_id)
        .bind(now)
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok((revision.max(0) as u64, users))
    }

    pub async fn insert_interface_snapshots(
        &self,
        node_id: &str,
        boot_id: &str,
        sequence: u64,
        sampled_at: u64,
        interfaces: &[(String, u64, u64)],
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        for (name, rx, tx_bytes) in interfaces {
            sqlx::query(
                "INSERT OR IGNORE INTO interface_snapshots
                    (node_id, boot_id, interface_name, sequence, sampled_at,
                     rx_absolute, tx_absolute)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(node_id)
            .bind(boot_id)
            .bind(name)
            .bind(sequence as i64)
            .bind(sampled_at as i64)
            .bind((*rx).min(i64::MAX as u64) as i64)
            .bind((*tx_bytes).min(i64::MAX as u64) as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_system_snapshot(
        &self,
        node_id: &str,
        sequence: u64,
        sampled_at: u64,
        cpu_usage_basis_points: u32,
        load_1_milli: u64,
        load_5_milli: u64,
        load_15_milli: u64,
        memory_total_bytes: u64,
        memory_used_bytes: u64,
        disk_total_bytes: u64,
        disk_used_bytes: u64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT OR IGNORE INTO system_snapshots
                (node_id, sequence, sampled_at, cpu_usage_basis_points,
                 load_1_milli, load_5_milli, load_15_milli,
                 memory_total_bytes, memory_used_bytes,
                 disk_total_bytes, disk_used_bytes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(node_id)
        .bind(sequence as i64)
        .bind(sampled_at as i64)
        .bind(cpu_usage_basis_points as i64)
        .bind(load_1_milli.min(i64::MAX as u64) as i64)
        .bind(load_5_milli.min(i64::MAX as u64) as i64)
        .bind(load_15_milli.min(i64::MAX as u64) as i64)
        .bind(memory_total_bytes.min(i64::MAX as u64) as i64)
        .bind(memory_used_bytes.min(i64::MAX as u64) as i64)
        .bind(disk_total_bytes.min(i64::MAX as u64) as i64)
        .bind(disk_used_bytes.min(i64::MAX as u64) as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_xray_traffic_event(
        &self,
        event_id: &str,
        agent_id: &str,
        node_id: &str,
        subscription_id: &str,
        xray_instance_id: &str,
        sequence: u64,
        interval_start: u64,
        interval_end: u64,
        uplink_delta: u64,
        downlink_delta: u64,
        received_at: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT OR IGNORE INTO xray_traffic_events
                (event_id, agent_id, node_id, subscription_id, xray_instance_id,
                 sequence, interval_start, interval_end, uplink_delta,
                 downlink_delta, received_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event_id)
        .bind(agent_id)
        .bind(node_id)
        .bind(subscription_id)
        .bind(xray_instance_id)
        .bind(sequence as i64)
        .bind(interval_start as i64)
        .bind(interval_end as i64)
        .bind(uplink_delta.min(i64::MAX as u64) as i64)
        .bind(downlink_delta.min(i64::MAX as u64) as i64)
        .bind(received_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_user_subscriptions(
        &self,
        user_id: &str,
    ) -> Result<Vec<models::SubscriptionRecord>, StorageError> {
        let rows = sqlx::query_as::<_, models::SubscriptionRecord>(
            "SELECT id, user_id, name, xray_uuid, xray_email, status,
                    starts_at, expires_at, traffic_limit_bytes,
                    traffic_multiplier_basis_points, reset_policy, reset_anchor,
                    current_cycle_start, current_cycle_end
             FROM subscriptions WHERE user_id = ? ORDER BY created_at ASC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn user_detail(
        &self,
        user_id: &str,
    ) -> Result<Option<models::UserDetail>, StorageError> {
        let user = sqlx::query_as::<_, models::UserSummary>(
            "SELECT u.id, u.username, u.display_name, u.status,
                    COUNT(s.id) AS subscription_count,
                    COALESCE(SUM(
                        (SELECT COALESCE(SUM(a.uplink_bytes + a.downlink_bytes), 0)
                         FROM xray_traffic_aggregates a
                         WHERE a.subscription_id = s.id
                           AND a.granularity = 'cycle'
                           AND a.bucket_start = s.current_cycle_start)
                        * s.traffic_multiplier_basis_points / 10000
                    ), 0) AS charged_bytes,
                    CASE
                        WHEN SUM(CASE
                            WHEN s.id IS NOT NULL AND s.traffic_limit_bytes IS NULL THEN 1
                            ELSE 0
                        END) > 0 THEN NULL
                        ELSE SUM(s.traffic_limit_bytes)
                    END AS traffic_limit_bytes,
                    COALESCE(SUM(CASE
                        WHEN s.expires_at IS NOT NULL
                             AND s.expires_at <= CAST(strftime('%s', 'now') AS INTEGER) THEN 1
                        ELSE 0
                    END), 0) AS expired_subscriptions
             FROM users u
             LEFT JOIN subscriptions s ON s.user_id = u.id
             WHERE u.id = ?
             GROUP BY u.id, u.username, u.display_name, u.status",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(user) = user else {
            return Ok(None);
        };
        let subscriptions = self.list_user_subscriptions(user_id).await?;
        let mut node_usage = Vec::new();
        let mut nic_usage = Vec::new();
        let mut nic_bindings = Vec::new();
        for subscription in &subscriptions {
            let rows = sqlx::query_as::<_, (String, String, i64, i64)>(
                "SELECT n.id, n.name,
                        COALESCE(SUM(a.uplink_bytes), 0),
                        COALESCE(SUM(a.downlink_bytes), 0)
                 FROM subscription_nodes sn
                 INNER JOIN nodes n ON n.id = sn.node_id
                 LEFT JOIN xray_traffic_aggregates a
                   ON a.subscription_id = sn.subscription_id
                  AND a.node_id = sn.node_id
                  AND a.granularity = 'cycle'
                  AND a.bucket_start = ?
                 WHERE sn.subscription_id = ? AND sn.enabled = 1
                 GROUP BY n.id, n.name
                 ORDER BY n.name ASC",
            )
            .bind(subscription.current_cycle_start)
            .bind(&subscription.id)
            .fetch_all(&self.pool)
            .await?;
            for (node_id, node_name, uplink, downlink) in rows {
                let raw = uplink.max(0).saturating_add(downlink.max(0));
                let charged = raw
                    .saturating_mul(subscription.traffic_multiplier_basis_points.max(0))
                    / 10_000;
                node_usage.push(models::UserNodeUsage {
                    subscription_id: subscription.id.clone(),
                    subscription_name: subscription.name.clone(),
                    node_id,
                    node_name,
                    uplink_bytes: uplink.max(0),
                    downlink_bytes: downlink.max(0),
                    charged_bytes: charged,
                });
            }
            let bindings = self.subscription_nic_bindings(&subscription.id).await?;
            if !bindings.is_empty() {
                let used_bytes = bindings
                    .iter()
                    .map(|binding| binding.used_bytes)
                    .fold(0_i64, i64::saturating_add);
                let limit_bytes = bindings
                    .iter()
                    .map(|binding| binding.traffic_limit_bytes)
                    .fold(0_i64, i64::saturating_add);
                nic_usage.push(models::SubscriptionNicUsage {
                    subscription_id: subscription.id.clone(),
                    used_bytes,
                    limit_bytes,
                });
                nic_bindings.extend(bindings);
            }
        }
        Ok(Some(models::UserDetail {
            user,
            subscriptions,
            node_usage,
            nic_usage,
            nic_bindings,
        }))
    }

    pub async fn subscription_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<models::SubscriptionRecord>, StorageError> {
        let row = sqlx::query_as::<_, models::SubscriptionRecord>(
            "SELECT id, user_id, name, xray_uuid, xray_email, status,
                    starts_at, expires_at, traffic_limit_bytes,
                    traffic_multiplier_basis_points, reset_policy, reset_anchor,
                    current_cycle_start, current_cycle_end
             FROM subscriptions
             WHERE token_hash = ? AND status = 'active'",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn subscription_nodes(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<models::NodeRecord>, StorageError> {
        let rows = sqlx::query_as::<_, models::NodeRecord>(
            "SELECT n.id, n.name, n.landing_host, n.xray_listen_port,
                    n.publish_host, n.publish_port, n.protocol, n.transport,
                    n.security, n.server_name, n.reality_public_key,
                    n.reality_short_id, n.reality_fingerprint
             FROM nodes n
             INNER JOIN subscription_nodes sn ON sn.node_id = n.id
             WHERE sn.subscription_id = ? AND sn.enabled = 1
               AND n.management_status = 'active'
             ORDER BY sn.sort_order ASC, n.name ASC",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn subscription_xray_usage(
        &self,
        subscription_id: &str,
        starts_at: i64,
    ) -> Result<(i64, i64), StorageError> {
        let row = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COALESCE(SUM(uplink_bytes), 0),
                    COALESCE(SUM(downlink_bytes), 0)
             FROM xray_traffic_aggregates
             WHERE subscription_id = ? AND granularity = 'cycle'
               AND bucket_start = ?",
        )
        .bind(subscription_id)
        .bind(starts_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn xray_traffic_history(
        &self,
        subscription_id: &str,
        granularity: &str,
        from: i64,
        until: i64,
    ) -> Result<Vec<models::TrafficAggregate>, StorageError> {
        if !matches!(granularity, "hour" | "day" | "cycle") || until < from {
            return Err(StorageError::Validation(
                "invalid traffic history range".into(),
            ));
        }
        Ok(sqlx::query_as::<_, models::TrafficAggregate>(
            "SELECT granularity, subscription_id, node_id, bucket_start,
                    bucket_end, uplink_bytes, downlink_bytes, event_count
             FROM xray_traffic_aggregates
             WHERE subscription_id = ? AND granularity = ?
               AND bucket_start >= ? AND bucket_start < ?
             ORDER BY bucket_start ASC, node_id ASC",
        )
        .bind(subscription_id)
        .bind(granularity)
        .bind(from)
        .bind(until)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn nic_traffic_history(
        &self,
        node_id: &str,
        interface_name: &str,
        granularity: &str,
        from: i64,
        until: i64,
    ) -> Result<Vec<models::NicTrafficAggregate>, StorageError> {
        if !matches!(granularity, "hour" | "day") || until < from {
            return Err(StorageError::Validation(
                "invalid NIC traffic history range".into(),
            ));
        }
        Ok(sqlx::query_as::<_, models::NicTrafficAggregate>(
            "SELECT granularity, node_id, interface_name, bucket_start,
                    bucket_end, rx_bytes, tx_bytes, sample_count
             FROM nic_traffic_aggregates
             WHERE node_id = ? AND interface_name = ? AND granularity = ?
               AND bucket_start >= ? AND bucket_start < ?
             ORDER BY bucket_start ASC",
        )
        .bind(node_id)
        .bind(interface_name)
        .bind(granularity)
        .bind(from)
        .bind(until)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn prune_traffic_history(
        &self,
        now: i64,
        raw_event_days: u32,
        interface_snapshot_days: u32,
        system_snapshot_days: u32,
        hourly_aggregate_days: u32,
        daily_aggregate_days: u32,
    ) -> Result<models::TrafficPruneResult, StorageError> {
        if raw_event_days == 0 || interface_snapshot_days == 0 || system_snapshot_days == 0 {
            return Err(StorageError::Validation(
                "raw and snapshot retention must be at least one day".into(),
            ));
        }
        let cutoff = |days: u32| now.saturating_sub(i64::from(days).saturating_mul(86_400));
        let mut tx = self.pool.begin().await?;
        let xray_events = sqlx::query("DELETE FROM xray_traffic_events WHERE interval_end < ?")
            .bind(cutoff(raw_event_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let interface_snapshots =
            sqlx::query("DELETE FROM interface_snapshots WHERE sampled_at < ?")
                .bind(cutoff(interface_snapshot_days))
                .execute(&mut *tx)
                .await?
                .rows_affected();
        let system_snapshots = sqlx::query("DELETE FROM system_snapshots WHERE sampled_at < ?")
            .bind(cutoff(system_snapshot_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let mut hourly_aggregates = 0;
        let mut daily_aggregates = 0;
        if hourly_aggregate_days > 0 {
            let xray = sqlx::query(
                "DELETE FROM xray_traffic_aggregates
                 WHERE granularity = 'hour' AND bucket_start < ?",
            )
            .bind(cutoff(hourly_aggregate_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let nic = sqlx::query(
                "DELETE FROM nic_traffic_aggregates
                 WHERE granularity = 'hour' AND bucket_start < ?",
            )
            .bind(cutoff(hourly_aggregate_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            hourly_aggregates = xray.saturating_add(nic);
        }
        if daily_aggregate_days > 0 {
            let xray = sqlx::query(
                "DELETE FROM xray_traffic_aggregates
                 WHERE granularity = 'day' AND bucket_start < ?",
            )
            .bind(cutoff(daily_aggregate_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let nic = sqlx::query(
                "DELETE FROM nic_traffic_aggregates
                 WHERE granularity = 'day' AND bucket_start < ?",
            )
            .bind(cutoff(daily_aggregate_days))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            daily_aggregates = xray.saturating_add(nic);
        }
        tx.commit().await?;
        Ok(models::TrafficPruneResult {
            xray_events,
            interface_snapshots,
            system_snapshots,
            hourly_aggregates,
            daily_aggregates,
        })
    }

    pub async fn subscription_nic_usage(
        &self,
        subscription_id: &str,
    ) -> Result<Option<(i64, i64)>, StorageError> {
        let bindings = self.subscription_nic_bindings(subscription_id).await?;
        if bindings.is_empty() {
            return Ok(None);
        }
        let used_bytes = bindings
            .iter()
            .map(|binding| binding.used_bytes)
            .fold(0_i64, i64::saturating_add);
        let limit_bytes = bindings
            .iter()
            .map(|binding| binding.traffic_limit_bytes)
            .fold(0_i64, i64::saturating_add);
        Ok(Some((used_bytes, limit_bytes)))
    }

    pub async fn subscription_nic_bindings(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<models::NicBindingRecord>, StorageError> {
        let mut bindings = sqlx::query_as::<_, models::NicBindingRecord>(
            "SELECT id, subscription_id, node_id, interface_name,
                    billing_direction, traffic_limit_bytes, initial_used_bytes,
                    reset_policy, reset_anchor, bound_at,
                    current_cycle_start, current_cycle_end, 0 AS used_bytes
             FROM nic_bindings
             WHERE subscription_id = ? AND enabled = 1 AND unbound_at IS NULL
             ORDER BY bound_at ASC, id ASC",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        for binding in &mut bindings {
            binding.used_bytes = self.nic_binding_used_bytes(binding).await?;
        }
        Ok(bindings)
    }

    pub async fn add_nic_binding(
        &self,
        subscription_id: &str,
        binding: &models::NewNicBinding,
        now: i64,
    ) -> Result<(), StorageError> {
        if binding.interface_name.trim().is_empty()
            || binding.traffic_limit_bytes <= 0
            || binding.initial_used_bytes < 0
            || !matches!(
                binding.billing_direction.as_str(),
                "rx_tx" | "tx_only" | "rx_only"
            )
        {
            return Err(StorageError::Validation(
                "invalid NIC binding settings".into(),
            ));
        }
        xenon_domain::ResetPolicy::from_stored(&binding.reset_policy, binding.reset_anchor)
            .map_err(|error| StorageError::Validation(error.to_string()))?;
        if binding
            .current_cycle_end
            .is_some_and(|end| end <= binding.current_cycle_start)
        {
            return Err(StorageError::Validation("invalid NIC billing cycle".into()));
        }
        let mut tx = self.pool.begin().await?;
        let selected = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM subscription_nodes
                WHERE subscription_id = ? AND node_id = ? AND enabled = 1
            )",
        )
        .bind(subscription_id)
        .bind(&binding.node_id)
        .fetch_one(&mut *tx)
        .await?;
        if selected != 1 {
            tx.rollback().await?;
            return Err(StorageError::Validation(
                "NIC node is not selected by subscription".into(),
            ));
        }
        let interface_exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM interface_snapshots
                WHERE node_id = ? AND interface_name = ?
            )",
        )
        .bind(&binding.node_id)
        .bind(&binding.interface_name)
        .fetch_one(&mut *tx)
        .await?;
        if interface_exists != 1 {
            tx.rollback().await?;
            return Err(StorageError::Validation(
                "interface has not been reported".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO nic_bindings
                (id, subscription_id, node_id, interface_name,
                 billing_direction, traffic_limit_bytes, initial_used_bytes,
                 reset_policy, reset_anchor, bound_at, enabled,
                 current_cycle_start, current_cycle_end)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&binding.id)
        .bind(subscription_id)
        .bind(&binding.node_id)
        .bind(&binding.interface_name)
        .bind(&binding.billing_direction)
        .bind(binding.traffic_limit_bytes)
        .bind(binding.initial_used_bytes)
        .bind(&binding.reset_policy)
        .bind(binding.reset_anchor)
        .bind(now)
        .bind(binding.current_cycle_start)
        .bind(binding.current_cycle_end)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn unbind_nic_binding(
        &self,
        binding_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "UPDATE nic_bindings
             SET enabled = 0, unbound_at = ?
             WHERE id = ? AND enabled = 1 AND unbound_at IS NULL",
        )
        .bind(now)
        .bind(binding_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn reset_nic_binding_cycle(
        &self,
        binding_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let record = sqlx::query_as::<_, (String, Option<i64>, i64)>(
            "SELECT reset_policy, reset_anchor, bound_at
             FROM nic_bindings
             WHERE id = ? AND enabled = 1 AND unbound_at IS NULL",
        )
        .bind(binding_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((policy_name, anchor, bound_at)) = record else {
            tx.rollback().await?;
            return Ok(false);
        };
        let policy = xenon_domain::ResetPolicy::from_stored(&policy_name, anchor)
            .map_err(|error| StorageError::Validation(error.to_string()))?;
        let cycle_end = policy
            .cycle_at(bound_at, now)
            .map_err(|error| StorageError::Validation(error.to_string()))?
            .end;
        let updated = sqlx::query(
            "UPDATE nic_bindings
             SET current_cycle_start = ?, current_cycle_end = ?
             WHERE id = ? AND enabled = 1 AND unbound_at IS NULL",
        )
        .bind(now)
        .bind(cycle_end)
        .bind(binding_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 1 {
            Self::rebuild_nic_binding_cycle(&mut tx, binding_id).await?;
            tx.commit().await?;
            Ok(true)
        } else {
            tx.rollback().await?;
            Ok(false)
        }
    }

    async fn rebuild_nic_binding_cycle(
        connection: &mut SqliteConnection,
        binding_id: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM nic_binding_cycle_aggregates
             WHERE binding_id = ? AND cycle_start = (
                 SELECT current_cycle_start FROM nic_bindings WHERE id = ?
             )",
        )
        .bind(binding_id)
        .bind(binding_id)
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "WITH ordered AS (
                SELECT snapshots.sampled_at, snapshots.rx_absolute, snapshots.tx_absolute,
                       LAG(snapshots.rx_absolute) OVER (
                           PARTITION BY snapshots.boot_id ORDER BY snapshots.sequence
                       ) AS previous_rx,
                       LAG(snapshots.tx_absolute) OVER (
                           PARTITION BY snapshots.boot_id ORDER BY snapshots.sequence
                       ) AS previous_tx
                FROM interface_snapshots snapshots
                INNER JOIN nic_bindings binding
                    ON binding.node_id = snapshots.node_id
                   AND binding.interface_name = snapshots.interface_name
                WHERE binding.id = ?
             ), deltas AS (
                SELECT sampled_at,
                       CASE WHEN rx_absolute >= previous_rx
                            THEN rx_absolute - previous_rx ELSE 0 END AS rx_delta,
                       CASE WHEN tx_absolute >= previous_tx
                            THEN tx_absolute - previous_tx ELSE 0 END AS tx_delta
                FROM ordered
                WHERE previous_rx IS NOT NULL AND previous_tx IS NOT NULL
             )
             INSERT INTO nic_binding_cycle_aggregates
                (binding_id, cycle_start, rx_bytes, tx_bytes, sample_count, updated_at)
             SELECT binding.id, binding.current_cycle_start,
                    SUM(deltas.rx_delta), SUM(deltas.tx_delta), COUNT(*), MAX(deltas.sampled_at)
             FROM nic_bindings binding
             INNER JOIN deltas
                ON deltas.sampled_at > MAX(binding.bound_at, binding.current_cycle_start)
             WHERE binding.id = ? AND binding.enabled = 1 AND binding.unbound_at IS NULL
             GROUP BY binding.id, binding.current_cycle_start",
        )
        .bind(binding_id)
        .bind(binding_id)
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    async fn nic_binding_used_bytes(
        &self,
        binding: &models::NicBindingRecord,
    ) -> Result<i64, StorageError> {
        let mut used_bytes = if binding.current_cycle_start <= binding.bound_at {
            binding.initial_used_bytes.max(0)
        } else {
            0
        };
        let aggregate = sqlx::query_as::<_, (i64, i64)>(
            "SELECT rx_bytes, tx_bytes
             FROM nic_binding_cycle_aggregates
             WHERE binding_id = ? AND cycle_start = ?",
        )
        .bind(&binding.id)
        .bind(binding.current_cycle_start)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((rx, tx)) = aggregate {
            let measured = match binding.billing_direction.as_str() {
                "tx_only" => tx.max(0),
                "rx_only" => rx.max(0),
                _ => rx.max(0).saturating_add(tx.max(0)),
            };
            used_bytes = used_bytes.saturating_add(measured);
        }
        Ok(used_bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register_agent_with_token(
        &self,
        token_hash: &str,
        agent_id: &str,
        node_id: &str,
        agent_version: &str,
        xray_version: &str,
        max_supported_xray_version: &str,
        certificate_fingerprint: Option<&str>,
        now: i64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_conflict = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM agents WHERE agent_id = ? AND node_id != ?
             )",
        )
        .bind(agent_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if identity_conflict == 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        if let Some(fingerprint) = certificate_fingerprint {
            let certificate_conflict = sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(
                    SELECT 1 FROM agent_certificates
                    WHERE fingerprint_sha256 = ?
                      AND (agent_id != ? OR node_id != ?)
                 )",
            )
            .bind(fingerprint.to_ascii_lowercase())
            .bind(agent_id)
            .bind(node_id)
            .fetch_one(&mut *tx)
            .await?;
            if certificate_conflict == 1 {
                tx.rollback().await?;
                return Ok(false);
            }
        }
        let consumed = sqlx::query(
            "UPDATE registration_tokens
             SET consumed_at = ?, consumed_agent_id = ?
             WHERE token_hash = ? AND node_id = ?
               AND consumed_at IS NULL AND expires_at >= ?",
        )
        .bind(now)
        .bind(agent_id)
        .bind(token_hash)
        .bind(node_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if consumed.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO agents
                (agent_id, node_id, status, agent_version, xray_version,
                 max_supported_xray_version, last_seen_at, registered_at, updated_at)
             VALUES (?, ?, 'online', ?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_id) DO UPDATE SET
                 node_id = excluded.node_id,
                 status = 'online',
                 agent_version = excluded.agent_version,
                 xray_version = excluded.xray_version,
                 max_supported_xray_version = excluded.max_supported_xray_version,
                 last_seen_at = excluded.last_seen_at,
                 updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(agent_version)
        .bind(xray_version)
        .bind(max_supported_xray_version)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if let Some(fingerprint) = certificate_fingerprint {
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                tx.rollback().await?;
                return Err(StorageError::Validation(
                    "invalid client certificate SHA-256 fingerprint".into(),
                ));
            }
            sqlx::query(
                "INSERT OR IGNORE INTO agent_certificates
                    (fingerprint_sha256, agent_id, node_id, issued_at,
                     certificate_pem, public_key_sha256, activated_at)
                 VALUES (?, ?, ?, ?, NULL, NULL, ?)",
            )
            .bind(fingerprint.to_ascii_lowercase())
            .bind(agent_id)
            .bind(node_id)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn enroll_agent_with_token(
        &self,
        token_hash: &str,
        agent_id: &str,
        node_id: &str,
        agent_version: &str,
        xray_version: &str,
        max_supported_xray_version: &str,
        certificate_fingerprint: &str,
        certificate_pem: &str,
        public_key_sha256: &str,
        certificate_expires_at: i64,
        now: i64,
    ) -> Result<Option<models::EnrollmentCertificate>, StorageError> {
        if certificate_fingerprint.len() != 64
            || !certificate_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || public_key_sha256.len() != 64
            || !public_key_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || certificate_pem.trim().is_empty()
            || certificate_expires_at <= now
        {
            return Err(StorageError::Validation(
                "invalid enrollment certificate data".into(),
            ));
        }
        let fingerprint = certificate_fingerprint.to_ascii_lowercase();
        let public_key_sha256 = public_key_sha256.to_ascii_lowercase();
        let mut tx = self.pool.begin().await?;
        let token = sqlx::query_as::<_, (i64, Option<i64>, Option<String>)>(
            "SELECT expires_at, consumed_at, consumed_agent_id
             FROM registration_tokens WHERE token_hash = ? AND node_id = ?",
        )
        .bind(token_hash)
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((token_expires_at, consumed_at, consumed_agent_id)) = token else {
            tx.rollback().await?;
            return Ok(None);
        };
        if token_expires_at < now {
            tx.rollback().await?;
            return Ok(None);
        }
        if consumed_at.is_some() {
            if consumed_agent_id.as_deref() != Some(agent_id) {
                tx.rollback().await?;
                return Ok(None);
            }
            let existing = sqlx::query_as::<_, (String, Option<String>, i64)>(
                "SELECT fingerprint_sha256, certificate_pem, expires_at
                 FROM agent_certificates
                 WHERE agent_id = ? AND node_id = ? AND public_key_sha256 = ?
                 ORDER BY issued_at DESC LIMIT 1",
            )
            .bind(agent_id)
            .bind(node_id)
            .bind(&public_key_sha256)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((fingerprint, Some(certificate_pem), expires_at)) = existing else {
                tx.rollback().await?;
                return Ok(None);
            };
            tx.commit().await?;
            return Ok(Some(models::EnrollmentCertificate {
                fingerprint_sha256: fingerprint,
                certificate_pem,
                expires_at,
            }));
        }
        let identity_conflict = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM agents WHERE agent_id = ? AND node_id != ?
             )",
        )
        .bind(agent_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if identity_conflict == 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        let certificate_conflict = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM agent_certificates
                WHERE fingerprint_sha256 = ?
                  AND (agent_id != ? OR node_id != ?)
             )",
        )
        .bind(&fingerprint)
        .bind(agent_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if certificate_conflict == 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        let consumed = sqlx::query(
            "UPDATE registration_tokens
             SET consumed_at = ?, consumed_agent_id = ?
             WHERE token_hash = ? AND node_id = ?
               AND consumed_at IS NULL AND expires_at >= ?",
        )
        .bind(now)
        .bind(agent_id)
        .bind(token_hash)
        .bind(node_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if consumed.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        sqlx::query(
            "INSERT INTO agents
                (agent_id, node_id, status, agent_version, xray_version,
                 max_supported_xray_version, last_seen_at, registered_at, updated_at)
             VALUES (?, ?, 'online', ?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_id) DO UPDATE SET
                 status = 'online', agent_version = excluded.agent_version,
                 xray_version = excluded.xray_version,
                 max_supported_xray_version = excluded.max_supported_xray_version,
                 last_seen_at = excluded.last_seen_at, updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(agent_version)
        .bind(xray_version)
        .bind(max_supported_xray_version)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO agent_certificates
                (fingerprint_sha256, agent_id, node_id, issued_at,
                 expires_at, certificate_pem, public_key_sha256, activated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fingerprint)
        .bind(agent_id)
        .bind(node_id)
        .bind(now)
        .bind(certificate_expires_at)
        .bind(certificate_pem)
        .bind(&public_key_sha256)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(models::EnrollmentCertificate {
            fingerprint_sha256: fingerprint,
            certificate_pem: certificate_pem.to_string(),
            expires_at: certificate_expires_at,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn rotate_agent_certificate(
        &self,
        agent_id: &str,
        node_id: &str,
        current_fingerprint: &str,
        new_fingerprint: &str,
        certificate_pem: &str,
        public_key_sha256: &str,
        certificate_expires_at: i64,
        now: i64,
    ) -> Result<Option<models::EnrollmentCertificate>, StorageError> {
        let mut tx = self.pool.begin().await?;
        let current_is_valid = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM agent_certificates
                WHERE agent_id = ? AND node_id = ? AND fingerprint_sha256 = ?
                  AND revoked_at IS NULL
                  AND (expires_at IS NULL OR expires_at >= ?)
             )",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(current_fingerprint.to_ascii_lowercase())
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if current_is_valid != 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        let existing = sqlx::query_as::<_, (String, Option<String>, i64)>(
            "SELECT fingerprint_sha256, certificate_pem, expires_at
             FROM agent_certificates
             WHERE agent_id = ? AND node_id = ? AND public_key_sha256 = ?
             ORDER BY issued_at DESC LIMIT 1",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(public_key_sha256.to_ascii_lowercase())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((fingerprint, Some(certificate_pem), expires_at)) = existing {
            tx.commit().await?;
            return Ok(Some(models::EnrollmentCertificate {
                fingerprint_sha256: fingerprint,
                certificate_pem,
                expires_at,
            }));
        }
        if certificate_expires_at <= now
            || new_fingerprint.len() != 64
            || public_key_sha256.len() != 64
        {
            tx.rollback().await?;
            return Err(StorageError::Validation(
                "invalid rotated certificate data".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO agent_certificates
                (fingerprint_sha256, agent_id, node_id, issued_at, expires_at,
                 certificate_pem, public_key_sha256, activated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(new_fingerprint.to_ascii_lowercase())
        .bind(agent_id)
        .bind(node_id)
        .bind(now)
        .bind(certificate_expires_at)
        .bind(certificate_pem)
        .bind(public_key_sha256.to_ascii_lowercase())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(models::EnrollmentCertificate {
            fingerprint_sha256: new_fingerprint.to_ascii_lowercase(),
            certificate_pem: certificate_pem.to_string(),
            expires_at: certificate_expires_at,
        }))
    }

    pub async fn activate_agent_certificate(
        &self,
        agent_id: &str,
        node_id: &str,
        fingerprint: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let pending = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1 FROM agent_certificates
                WHERE agent_id = ? AND node_id = ? AND fingerprint_sha256 = ?
                  AND revoked_at IS NULL AND activated_at IS NULL
                  AND (expires_at IS NULL OR expires_at >= ?)
             )",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(fingerprint.to_ascii_lowercase())
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if pending != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE agent_certificates SET revoked_at = ?
             WHERE agent_id = ? AND node_id = ? AND fingerprint_sha256 != ?
               AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(agent_id)
        .bind(node_id)
        .bind(fingerprint.to_ascii_lowercase())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_certificates SET activated_at = ?
             WHERE agent_id = ? AND node_id = ? AND fingerprint_sha256 = ?",
        )
        .bind(now)
        .bind(agent_id)
        .bind(node_id)
        .bind(fingerprint.to_ascii_lowercase())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn revoke_node_certificates(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<u64, StorageError> {
        let result = sqlx::query(
            "UPDATE agent_certificates SET revoked_at = ?
             WHERE node_id = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE agents SET status = 'revoked', updated_at = ? WHERE node_id = ?")
            .bind(now)
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn ensure_development_node(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT OR IGNORE INTO nodes
                (id, name, landing_host, xray_listen_port, status, created_at, updated_at)
             VALUES (?, ?, '127.0.0.1', 443, 'development', ?, ?)",
        )
        .bind(node_id)
        .bind(format!("dev-{node_id}"))
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn registered_agent_matches(
        &self,
        agent_id: &str,
        node_id: &str,
    ) -> Result<bool, StorageError> {
        let row = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM agents WHERE agent_id = ? AND node_id = ?)",
        )
        .bind(agent_id)
        .bind(node_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row == 1)
    }

    pub async fn agent_certificate_matches(
        &self,
        agent_id: &str,
        node_id: &str,
        fingerprint: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let row = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(
                SELECT 1
                FROM agents a
                JOIN agent_certificates c ON c.agent_id = a.agent_id
                WHERE a.agent_id = ? AND a.node_id = ?
                  AND c.node_id = a.node_id
                  AND c.fingerprint_sha256 = ?
                  AND c.revoked_at IS NULL
                  AND (c.expires_at IS NULL OR c.expires_at >= ?)
             )",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(fingerprint.to_ascii_lowercase())
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row == 1)
    }

    pub async fn upsert_agent(
        &self,
        agent_id: &str,
        node_id: &str,
        agent_version: &str,
        xray_version: &str,
        max_supported_xray_version: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO agents
                (agent_id, node_id, status, agent_version, xray_version,
                 max_supported_xray_version, last_seen_at, registered_at, updated_at)
             VALUES (?, ?, 'online', ?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_id) DO UPDATE SET
                 node_id = excluded.node_id,
                 status = 'online',
                 agent_version = excluded.agent_version,
                 xray_version = excluded.xray_version,
                 max_supported_xray_version = excluded.max_supported_xray_version,
                 last_seen_at = excluded.last_seen_at,
                 updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(node_id)
        .bind(agent_version)
        .bind(xray_version)
        .bind(max_supported_xray_version)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_agent(
        &self,
        agent_id: &str,
        boot_id: Option<&str>,
        now: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE agents SET status = 'online', boot_id = COALESCE(?, boot_id),
             last_seen_at = ?, updated_at = ? WHERE agent_id = ?",
        )
        .bind(boot_id)
        .bind(now)
        .bind(now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE nodes SET status = 'online', updated_at = ?
             WHERE id = (SELECT node_id FROM agents WHERE agent_id = ?)",
        )
        .bind(now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_agent_offline(&self, agent_id: &str, now: i64) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE agents
             SET status = CASE WHEN status = 'revoked' THEN status ELSE 'offline' END,
                 updated_at = ?
             WHERE agent_id = ?",
        )
        .bind(now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE nodes SET status = 'offline', updated_at = ?
             WHERE id = (SELECT node_id FROM agents WHERE agent_id = ?)",
        )
        .bind(now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn missing_text(value: Option<&str>) -> bool {
    matches!(value, None | Some(""))
}

#[cfg(test)]
mod tests {
    use super::{models, Database};

    async fn test_database() -> (tempfile::TempDir, Database) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let database = Database::connect(temp.path().join("panel.db"))
            .await
            .expect("connect test database");
        (temp, database)
    }

    #[tokio::test]
    async fn bootstraps_admin_and_creates_multi_node_subscription() {
        let (_temp, database) = test_database().await;
        database.ensure_default_admin(10).await.expect("admin");
        database
            .ensure_development_node("node-a", 10)
            .await
            .expect("node a");
        database
            .ensure_development_node("node-b", 10)
            .await
            .expect("node b");
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-alice".into(),
                username: "alice".into(),
                subscription_id: "subscription-alice".into(),
                name: "Alice default".into(),
                token_hash: "token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000001".into(),
                xray_email: "sub-subscription-alice@panel".into(),
                starts_at: 10,
                expires_at: Some(100),
                traffic_limit_bytes: Some(1_073_741_824),
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-a".into(), "node-b".into()],
                nic_bindings: Vec::new(),
                created_at: 10,
            })
            .await
            .expect("create user subscription");

        let users = database.list_user_summaries(0).await.expect("list users");
        assert_eq!(users.len(), 2);
        let alice = users
            .iter()
            .find(|user| user.username == "alice")
            .expect("alice summary");
        assert_eq!(alice.subscription_count, 1);
        assert_eq!(alice.charged_bytes, 0);
        let subscriptions = database
            .list_user_subscriptions(&alice.id)
            .await
            .expect("list subscriptions");
        assert_eq!(subscriptions.len(), 1);
        let (revision, desired) = database
            .desired_xray_users_for_node("node-a", 20)
            .await
            .expect("desired users");
        assert_eq!(revision, 1);
        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].subscription_id, "subscription-alice");

        database
            .insert_xray_traffic_event(
                "event-over-quota",
                "agent-a",
                "node-a",
                "subscription-alice",
                "instance-a",
                1,
                20,
                21,
                1_073_741_824,
                1,
                21,
            )
            .await
            .expect("traffic event");
        let (_, desired) = database
            .desired_xray_users_for_node("node-b", 22)
            .await
            .expect("quota-filtered users");
        assert!(desired.is_empty());
    }

    #[tokio::test]
    async fn registration_token_is_consumed_once() {
        let (_temp, database) = test_database().await;
        let certificate_fingerprint = "a".repeat(64);
        database
            .ensure_development_node("node-registration", 10)
            .await
            .expect("node");
        database
            .create_registration_token(&models::NewRegistrationToken {
                id: "token-id".into(),
                node_id: "node-registration".into(),
                token_hash: "secret-hash".into(),
                expires_at: 100,
                created_at: 10,
            })
            .await
            .expect("create token");

        let accepted = database
            .register_agent_with_token(
                "secret-hash",
                "agent-a",
                "node-registration",
                "0.1.0",
                "not-configured",
                "26.6.27",
                Some(&certificate_fingerprint),
                20,
            )
            .await
            .expect("consume token");
        assert!(accepted);
        let repeated = database
            .register_agent_with_token(
                "secret-hash",
                "agent-b",
                "node-registration",
                "0.1.0",
                "not-configured",
                "26.6.27",
                None,
                21,
            )
            .await
            .expect("repeat token");
        assert!(!repeated);
        assert!(database
            .registered_agent_matches("agent-a", "node-registration")
            .await
            .expect("registered agent"));
        assert!(
            database
                .agent_certificate_matches(
                    "agent-a",
                    "node-registration",
                    &certificate_fingerprint,
                    22,
                )
                .await
                .expect("bound certificate")
        );
        assert!(!database
            .agent_certificate_matches("agent-a", "node-registration", &"b".repeat(64), 22,)
            .await
            .expect("unbound certificate"));
        database
            .touch_agent("agent-a", Some("boot-a"), 30)
            .await
            .expect("touch agent");
        database
            .insert_system_snapshot(
                "node-registration",
                1,
                30,
                1250,
                500,
                400,
                300,
                1024,
                512,
                2048,
                1024,
            )
            .await
            .expect("system snapshot");
        let overview = database.list_node_overviews().await.expect("node overview");
        assert_eq!(overview[0].node_status, "online");
        assert_eq!(overview[0].agent_status.as_deref(), Some("online"));
        assert_eq!(overview[0].cpu_usage_basis_points, Some(1250));
        assert_eq!(overview[0].memory_used_bytes, Some(512));
        database
            .mark_agent_offline("agent-a", 40)
            .await
            .expect("offline agent");
        let overview = database
            .list_node_overviews()
            .await
            .expect("offline overview");
        assert_eq!(overview[0].node_status, "offline");
    }

    #[tokio::test]
    async fn updates_subscription_and_node_revisions_atomically() {
        let (_temp, database) = test_database().await;
        for node_id in ["node-edit-a", "node-edit-b"] {
            database
                .ensure_development_node(node_id, 10)
                .await
                .expect("node");
        }
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-edit".into(),
                username: "edit-user".into(),
                subscription_id: "subscription-edit".into(),
                name: "Before edit".into(),
                token_hash: "edit-token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000005".into(),
                xray_email: "sub-edit@panel".into(),
                starts_at: 10,
                expires_at: None,
                traffic_limit_bytes: Some(100),
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-edit-a".into()],
                nic_bindings: Vec::new(),
                created_at: 10,
            })
            .await
            .expect("subscription");
        assert!(database
            .update_subscription(
                "subscription-edit",
                &models::UpdateSubscription {
                    name: "After edit".into(),
                    status: "disabled".into(),
                    expires_at: Some(1_000),
                    traffic_limit_bytes: Some(200),
                    traffic_multiplier_basis_points: 20_000,
                    reset_policy: "manual".into(),
                    reset_anchor: None,
                    current_cycle_end: None,
                    node_ids: vec!["node-edit-b".into()],
                    updated_at: 20,
                },
            )
            .await
            .expect("update subscription"));
        let record = database
            .list_user_subscriptions("user-edit")
            .await
            .expect("subscriptions")
            .pop()
            .expect("edited subscription");
        assert_eq!(record.name, "After edit");
        assert_eq!(record.status, "disabled");
        assert_eq!(record.traffic_limit_bytes, Some(200));
        assert_eq!(record.traffic_multiplier_basis_points, 20_000);
        assert_eq!(record.reset_policy, "manual");
        let nodes = database
            .subscription_nodes("subscription-edit")
            .await
            .expect("edited nodes");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "node-edit-b");
        for node_id in ["node-edit-a", "node-edit-b"] {
            let (revision, users) = database
                .desired_xray_users_for_node(node_id, 30)
                .await
                .expect("desired users");
            assert!(users.is_empty());
            assert_eq!(revision, if node_id == "node-edit-a" { 2 } else { 1 });
        }

        let invalid = database
            .update_subscription(
                "subscription-edit",
                &models::UpdateSubscription {
                    name: "Invalid".into(),
                    status: "active".into(),
                    expires_at: None,
                    traffic_limit_bytes: None,
                    traffic_multiplier_basis_points: 10_000,
                    reset_policy: "never".into(),
                    reset_anchor: None,
                    current_cycle_end: None,
                    node_ids: vec!["missing-node".into()],
                    updated_at: 40,
                },
            )
            .await;
        assert!(invalid.is_err());
        let nodes = database
            .subscription_nodes("subscription-edit")
            .await
            .expect("nodes after rollback");
        assert_eq!(nodes[0].id, "node-edit-b");
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM subscriptions WHERE id = 'subscription-edit'",
        )
        .fetch_one(database.pool())
        .await
        .expect("status after rollback");
        assert_eq!(status, "disabled");
    }

    #[tokio::test]
    async fn rotates_subscription_token_and_uuid_independently() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-rotate", 10)
            .await
            .expect("node");
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-rotate".into(),
                username: "rotate-user".into(),
                subscription_id: "subscription-rotate".into(),
                name: "Rotate subscription".into(),
                token_hash: "old-token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000006".into(),
                xray_email: "sub-rotate@panel".into(),
                starts_at: 10,
                expires_at: None,
                traffic_limit_bytes: None,
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-rotate".into()],
                nic_bindings: Vec::new(),
                created_at: 10,
            })
            .await
            .expect("subscription");
        let new_token_hash = "a".repeat(64);
        assert!(database
            .rotate_subscription_token("subscription-rotate", &new_token_hash, 20)
            .await
            .expect("rotate token"));
        assert!(database
            .subscription_by_token_hash("old-token-hash")
            .await
            .expect("old token lookup")
            .is_none());
        let record = database
            .subscription_by_token_hash(&new_token_hash)
            .await
            .expect("new token lookup")
            .expect("rotated token");
        assert_eq!(record.xray_uuid, "01900000-0000-7000-8000-000000000006");

        let new_uuid = "01900000-0000-7000-8000-000000000007";
        assert!(database
            .rotate_subscription_uuid("subscription-rotate", new_uuid, 30)
            .await
            .expect("rotate UUID"));
        let (revision, desired) = database
            .desired_xray_users_for_node("node-rotate", 31)
            .await
            .expect("desired rotated user");
        assert_eq!(revision, 2);
        assert_eq!(desired[0].xray_uuid, new_uuid);
        assert!(database
            .rotate_subscription_uuid("subscription-rotate", "invalid", 40)
            .await
            .is_err());
        let (_, desired) = database
            .desired_xray_users_for_node("node-rotate", 41)
            .await
            .expect("desired user after rejected UUID");
        assert_eq!(desired[0].xray_uuid, new_uuid);
    }

    #[tokio::test]
    async fn manages_node_lifecycle_without_overwriting_runtime_status() {
        let (_temp, database) = test_database().await;
        for node_id in ["node-life-a", "node-life-b"] {
            database
                .ensure_development_node(node_id, 10)
                .await
                .expect("node");
        }
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-life".into(),
                username: "life-user".into(),
                subscription_id: "subscription-life".into(),
                name: "Lifecycle subscription".into(),
                token_hash: "lifecycle-token".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000008".into(),
                xray_email: "sub-life@panel".into(),
                starts_at: 10,
                expires_at: None,
                traffic_limit_bytes: None,
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-life-a".into()],
                nic_bindings: Vec::new(),
                created_at: 10,
            })
            .await
            .expect("subscription");
        database
            .upsert_agent(
                "agent-life",
                "node-life-a",
                "0.1.0",
                "26.6.27",
                "26.6.27",
                19,
            )
            .await
            .expect("agent");
        assert!(database
            .set_node_management_status("node-life-a", "disabled", 20)
            .await
            .expect("disable node"));
        assert!(database
            .subscription_nodes("subscription-life")
            .await
            .expect("disabled subscription nodes")
            .is_empty());
        let (_, desired) = database
            .desired_xray_users_for_node("node-life-a", 21)
            .await
            .expect("disabled desired users");
        assert!(desired.is_empty());
        database
            .touch_agent("agent-life", None, 22)
            .await
            .expect("unmatched heartbeat is harmless");
        let status = sqlx::query_scalar::<_, String>(
            "SELECT management_status FROM nodes WHERE id = 'node-life-a'",
        )
        .fetch_one(database.pool())
        .await
        .expect("management status");
        assert_eq!(status, "disabled");
        assert!(database
            .set_node_management_status("node-life-a", "active", 23)
            .await
            .expect("enable node"));
        assert_eq!(
            database
                .desired_xray_users_for_node("node-life-a", 24)
                .await
                .expect("enabled desired users")
                .1
                .len(),
            1
        );
        assert!(database
            .update_node(
                "node-life-a",
                &models::UpdateNode {
                    name: "Lifecycle A".into(),
                    landing_host: "origin.example.com".into(),
                    xray_listen_port: 443,
                    publish_host: Some("relay.example.com".into()),
                    publish_port: Some(8443),
                    security: "tls".into(),
                    server_name: Some("origin.example.com".into()),
                    reality_public_key: None,
                    reality_short_id: None,
                    reality_fingerprint: None,
                    updated_at: 25,
                },
            )
            .await
            .expect("edit node"));
        assert!(database.delete_node("node-life-a", 26).await.is_err());
        assert!(database
            .update_subscription(
                "subscription-life",
                &models::UpdateSubscription {
                    name: "Lifecycle subscription".into(),
                    status: "active".into(),
                    expires_at: None,
                    traffic_limit_bytes: None,
                    traffic_multiplier_basis_points: 10_000,
                    reset_policy: "never".into(),
                    reset_anchor: None,
                    current_cycle_end: None,
                    node_ids: vec!["node-life-b".into()],
                    updated_at: 27,
                },
            )
            .await
            .expect("move subscription"));
        assert!(database
            .delete_node("node-life-a", 28)
            .await
            .expect("delete node"));
        let overviews = database
            .list_node_overviews()
            .await
            .expect("node overviews");
        assert!(overviews.iter().all(|node| node.id != "node-life-a"));
    }

    #[tokio::test]
    async fn enrollment_is_idempotent_for_the_same_agent_public_key() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-enrollment", 10)
            .await
            .expect("node");
        database
            .create_registration_token(&models::NewRegistrationToken {
                id: "enrollment-token-id".into(),
                node_id: "node-enrollment".into(),
                token_hash: "enrollment-token-hash".into(),
                expires_at: 100,
                created_at: 10,
            })
            .await
            .expect("token");
        let fingerprint = "c".repeat(64);
        let public_key = "d".repeat(64);
        let first = database
            .enroll_agent_with_token(
                "enrollment-token-hash",
                "agent-enrollment",
                "node-enrollment",
                "0.1.0",
                "not-configured",
                "26.6.27",
                &fingerprint,
                "-----BEGIN CERTIFICATE-----\nfirst\n-----END CERTIFICATE-----",
                &public_key,
                90,
                20,
            )
            .await
            .expect("first enrollment")
            .expect("accepted");
        let retry = database
            .enroll_agent_with_token(
                "enrollment-token-hash",
                "agent-enrollment",
                "node-enrollment",
                "0.1.0",
                "not-configured",
                "26.6.27",
                &"e".repeat(64),
                "discarded retry certificate",
                &public_key,
                95,
                21,
            )
            .await
            .expect("retry enrollment")
            .expect("idempotent response");
        assert_eq!(retry.fingerprint_sha256, first.fingerprint_sha256);
        assert_eq!(retry.certificate_pem, first.certificate_pem);
        assert_eq!(retry.expires_at, first.expires_at);
        assert!(database
            .agent_certificate_matches("agent-enrollment", "node-enrollment", &fingerprint, 30,)
            .await
            .expect("certificate binding"));
        let rotated_fingerprint = "e".repeat(64);
        let rotated = database
            .rotate_agent_certificate(
                "agent-enrollment",
                "node-enrollment",
                &fingerprint,
                &rotated_fingerprint,
                "rotated certificate",
                &"f".repeat(64),
                120,
                31,
            )
            .await
            .expect("rotate certificate")
            .expect("rotation accepted");
        assert_eq!(rotated.fingerprint_sha256, rotated_fingerprint);
        assert!(database
            .agent_certificate_matches("agent-enrollment", "node-enrollment", &fingerprint, 32,)
            .await
            .expect("old certificate remains during rotation"));
        assert!(database
            .activate_agent_certificate(
                "agent-enrollment",
                "node-enrollment",
                &rotated_fingerprint,
                33,
            )
            .await
            .expect("activate rotated certificate"));
        assert!(!database
            .agent_certificate_matches("agent-enrollment", "node-enrollment", &fingerprint, 34,)
            .await
            .expect("old certificate revoked"));
        assert!(database
            .agent_certificate_matches(
                "agent-enrollment",
                "node-enrollment",
                &rotated_fingerprint,
                34,
            )
            .await
            .expect("rotated certificate active"));
        assert_eq!(
            database
                .revoke_node_certificates("node-enrollment", 35)
                .await
                .expect("emergency revoke"),
            1
        );
        assert!(!database
            .agent_certificate_matches(
                "agent-enrollment",
                "node-enrollment",
                &rotated_fingerprint,
                36,
            )
            .await
            .expect("emergency-revoked certificate"));
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM agents WHERE agent_id = 'agent-enrollment'",
        )
        .fetch_one(database.pool())
        .await
        .expect("revoked Agent status");
        assert_eq!(status, "revoked");
    }

    #[tokio::test]
    async fn nic_binding_validation_rolls_back_whole_creation() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-nic", 10)
            .await
            .expect("node");
        let input = models::NewSubscription {
            user_id: "user-nic".into(),
            username: "nic-user".into(),
            subscription_id: "subscription-nic".into(),
            name: "NIC subscription".into(),
            token_hash: "nic-token-hash".into(),
            xray_uuid: "01900000-0000-7000-8000-000000000002".into(),
            xray_email: "sub-nic@panel".into(),
            starts_at: 10,
            expires_at: None,
            traffic_limit_bytes: None,
            traffic_multiplier_basis_points: 10_000,
            reset_policy: "never".into(),
            reset_anchor: None,
            current_cycle_start: 10,
            current_cycle_end: None,
            node_ids: vec!["node-nic".into()],
            nic_bindings: vec![models::NewNicBinding {
                id: "binding-nic".into(),
                node_id: "node-nic".into(),
                interface_name: "eth0".into(),
                billing_direction: "rx_tx".into(),
                traffic_limit_bytes: 1000,
                initial_used_bytes: 25,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
            }],
            created_at: 10,
        };
        assert!(database.create_user_subscription(&input).await.is_err());
        let user_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE username = 'nic-user'")
                .fetch_one(database.pool())
                .await
                .expect("user count");
        assert_eq!(user_count, 0);

        database
            .insert_interface_snapshots("node-nic", "boot-nic", 1, 10, &[("eth0".into(), 100, 200)])
            .await
            .expect("interface");
        database
            .create_user_subscription(&input)
            .await
            .expect("create with binding");
        let binding_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM nic_bindings WHERE subscription_id = 'subscription-nic'",
        )
        .fetch_one(database.pool())
        .await
        .expect("binding count");
        assert_eq!(binding_count, 1);
    }

    #[tokio::test]
    async fn billing_cycle_reset_restores_quota_without_deleting_history() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-cycle", 10)
            .await
            .expect("node");
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-cycle".into(),
                username: "cycle-user".into(),
                subscription_id: "subscription-cycle".into(),
                name: "Cycle subscription".into(),
                token_hash: "cycle-token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000003".into(),
                xray_email: "sub-cycle@panel".into(),
                starts_at: 10,
                expires_at: None,
                traffic_limit_bytes: Some(100),
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "interval_days:1".into(),
                reset_anchor: Some(10),
                current_cycle_start: 10,
                current_cycle_end: Some(86_410),
                node_ids: vec!["node-cycle".into()],
                nic_bindings: Vec::new(),
                created_at: 10,
            })
            .await
            .expect("subscription");
        database
            .insert_xray_traffic_event(
                "cycle-event-old",
                "agent-cycle",
                "node-cycle",
                "subscription-cycle",
                "instance-cycle",
                1,
                20,
                30,
                80,
                30,
                30,
            )
            .await
            .expect("old traffic");
        let (_, desired) = database
            .desired_xray_users_for_node("node-cycle", 40)
            .await
            .expect("quota state");
        assert!(desired.is_empty());
        let detail = database
            .user_detail("user-cycle")
            .await
            .expect("user detail")
            .expect("cycle user");
        assert_eq!(detail.user.charged_bytes, 110);
        assert_eq!(detail.node_usage.len(), 1);
        assert_eq!(detail.node_usage[0].uplink_bytes, 80);
        assert_eq!(detail.node_usage[0].downlink_bytes, 30);
        assert_eq!(detail.node_usage[0].charged_bytes, 110);
        assert!(detail.nic_usage.is_empty());

        let (_, desired) = database
            .desired_xray_users_for_node("node-cycle", 86_420)
            .await
            .expect("new cycle quota state");
        assert_eq!(desired.len(), 1);
        let cycle_start = sqlx::query_scalar::<_, i64>(
            "SELECT current_cycle_start FROM subscriptions WHERE id = 'subscription-cycle'",
        )
        .fetch_one(database.pool())
        .await
        .expect("cycle start");
        assert_eq!(cycle_start, 86_410);
        assert_eq!(
            database
                .list_user_summaries(0)
                .await
                .expect("new cycle summary")
                .into_iter()
                .find(|user| user.username == "cycle-user")
                .expect("cycle user")
                .charged_bytes,
            0
        );

        database
            .insert_xray_traffic_event(
                "cycle-event-new",
                "agent-cycle",
                "node-cycle",
                "subscription-cycle",
                "instance-cycle",
                2,
                86_430,
                86_440,
                12,
                8,
                86_440,
            )
            .await
            .expect("new traffic");
        assert!(database
            .reset_subscription_cycle("subscription-cycle", 86_500)
            .await
            .expect("manual reset"));
        let event_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM xray_traffic_events WHERE subscription_id = 'subscription-cycle'",
        )
        .fetch_one(database.pool())
        .await
        .expect("event count");
        assert_eq!(event_count, 2);
        assert_eq!(
            database
                .subscription_xray_usage("subscription-cycle", 86_500)
                .await
                .expect("reset usage"),
            (0, 0)
        );
        assert_eq!(
            database
                .user_detail("user-cycle")
                .await
                .expect("reset detail")
                .expect("cycle user")
                .user
                .charged_bytes,
            0
        );
    }

    #[tokio::test]
    async fn nic_usage_handles_directions_cycles_reboots_and_counter_resets() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-meter", 1)
            .await
            .expect("node");
        database
            .insert_interface_snapshots("node-meter", "boot-a", 1, 10, &[("eth0".into(), 100, 200)])
            .await
            .expect("baseline");
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-meter".into(),
                username: "meter-user".into(),
                subscription_id: "subscription-meter".into(),
                name: "Meter subscription".into(),
                token_hash: "meter-token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000004".into(),
                xray_email: "sub-meter@panel".into(),
                starts_at: 10,
                expires_at: None,
                traffic_limit_bytes: None,
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 10,
                current_cycle_end: None,
                node_ids: vec!["node-meter".into()],
                nic_bindings: vec![
                    models::NewNicBinding {
                        id: "binding-rx-tx".into(),
                        node_id: "node-meter".into(),
                        interface_name: "eth0".into(),
                        billing_direction: "rx_tx".into(),
                        traffic_limit_bytes: 1000,
                        initial_used_bytes: 5,
                        reset_policy: "never".into(),
                        reset_anchor: None,
                        current_cycle_start: 10,
                        current_cycle_end: None,
                    },
                    models::NewNicBinding {
                        id: "binding-tx".into(),
                        node_id: "node-meter".into(),
                        interface_name: "eth0".into(),
                        billing_direction: "tx_only".into(),
                        traffic_limit_bytes: 2000,
                        initial_used_bytes: 6,
                        reset_policy: "never".into(),
                        reset_anchor: None,
                        current_cycle_start: 10,
                        current_cycle_end: None,
                    },
                    models::NewNicBinding {
                        id: "binding-rx".into(),
                        node_id: "node-meter".into(),
                        interface_name: "eth0".into(),
                        billing_direction: "rx_only".into(),
                        traffic_limit_bytes: 3000,
                        initial_used_bytes: 7,
                        reset_policy: "never".into(),
                        reset_anchor: None,
                        current_cycle_start: 10,
                        current_cycle_end: None,
                    },
                ],
                created_at: 10,
            })
            .await
            .expect("subscription");
        for (boot, sequence, sampled_at, rx, tx) in [
            ("boot-a", 2, 20, 130, 250),
            ("boot-b", 1, 30, 10, 20),
            ("boot-b", 2, 40, 25, 45),
            ("boot-b", 3, 50, 5, 10),
            ("boot-b", 4, 60, 15, 20),
        ] {
            database
                .insert_interface_snapshots(
                    "node-meter",
                    boot,
                    sequence,
                    sampled_at,
                    &[("eth0".into(), rx, tx)],
                )
                .await
                .expect("snapshot");
        }
        assert_eq!(
            database
                .subscription_nic_usage("subscription-meter")
                .await
                .expect("NIC usage"),
            Some((298, 6000))
        );
        assert_eq!(
            database
                .subscription_nic_bindings("subscription-meter")
                .await
                .expect("binding details")
                .len(),
            3
        );
        assert!(database
            .reset_nic_binding_cycle("binding-rx-tx", 45)
            .await
            .expect("reset one NIC cycle"));
        assert_eq!(
            database
                .subscription_nic_usage("subscription-meter")
                .await
                .expect("one reset NIC usage"),
            Some((173, 6000))
        );
        assert!(database
            .unbind_nic_binding("binding-tx", 61)
            .await
            .expect("unbind NIC"));
        assert_eq!(
            database
                .subscription_nic_usage("subscription-meter")
                .await
                .expect("unbound NIC usage"),
            Some((82, 4000))
        );
        database
            .add_nic_binding(
                "subscription-meter",
                &models::NewNicBinding {
                    id: "binding-new".into(),
                    node_id: "node-meter".into(),
                    interface_name: "eth0".into(),
                    billing_direction: "tx_only".into(),
                    traffic_limit_bytes: 500,
                    initial_used_bytes: 11,
                    reset_policy: "manual".into(),
                    reset_anchor: None,
                    current_cycle_start: 60,
                    current_cycle_end: None,
                },
                60,
            )
            .await
            .expect("add NIC binding");
        assert_eq!(
            database
                .subscription_nic_usage("subscription-meter")
                .await
                .expect("added NIC usage"),
            Some((93, 4500))
        );
        assert!(database
            .reset_nic_binding_cycle("binding-new", 61)
            .await
            .expect("reset added NIC cycle"));
        assert_eq!(
            database
                .subscription_nic_usage("subscription-meter")
                .await
                .expect("reset added NIC usage"),
            Some((82, 4500))
        );
    }

    #[tokio::test]
    async fn aggregates_history_and_prunes_raw_data_without_losing_current_usage() {
        let (_temp, database) = test_database().await;
        database
            .ensure_development_node("node-history", 1)
            .await
            .expect("node");
        database
            .insert_interface_snapshots(
                "node-history",
                "boot-history",
                1,
                100,
                &[("eth0".into(), 100, 200)],
            )
            .await
            .expect("NIC baseline");
        database
            .create_user_subscription(&models::NewSubscription {
                user_id: "user-history".into(),
                username: "history-user".into(),
                subscription_id: "subscription-history".into(),
                name: "History subscription".into(),
                token_hash: "history-token-hash".into(),
                xray_uuid: "01900000-0000-7000-8000-000000000099".into(),
                xray_email: "sub-history@panel".into(),
                starts_at: 1,
                expires_at: None,
                traffic_limit_bytes: Some(10_000),
                traffic_multiplier_basis_points: 10_000,
                reset_policy: "never".into(),
                reset_anchor: None,
                current_cycle_start: 1,
                current_cycle_end: None,
                node_ids: vec!["node-history".into()],
                nic_bindings: vec![models::NewNicBinding {
                    id: "binding-history".into(),
                    node_id: "node-history".into(),
                    interface_name: "eth0".into(),
                    billing_direction: "rx_tx".into(),
                    traffic_limit_bytes: 20_000,
                    initial_used_bytes: 10,
                    reset_policy: "never".into(),
                    reset_anchor: None,
                    current_cycle_start: 1,
                    current_cycle_end: None,
                }],
                created_at: 1,
            })
            .await
            .expect("subscription");
        for (event_id, sequence, uplink, downlink) in [
            ("event-history-a", 1, 50, 70),
            ("event-history-b", 2, 30, 20),
        ] {
            database
                .insert_xray_traffic_event(
                    event_id,
                    "agent-history",
                    "node-history",
                    "subscription-history",
                    "instance-history",
                    sequence,
                    3_600,
                    3_700 + sequence,
                    uplink,
                    downlink,
                    3_800,
                )
                .await
                .expect("traffic event");
        }
        database
            .insert_xray_traffic_event(
                "event-history-a",
                "agent-history",
                "node-history",
                "subscription-history",
                "instance-history",
                1,
                3_600,
                3_701,
                50,
                70,
                3_900,
            )
            .await
            .expect("duplicate event");
        database
            .insert_interface_snapshots(
                "node-history",
                "boot-history",
                2,
                3_700,
                &[("eth0".into(), 160, 290)],
            )
            .await
            .expect("NIC delta");

        let hourly = database
            .xray_traffic_history("subscription-history", "hour", 0, 7_200)
            .await
            .expect("hour history");
        assert_eq!(hourly.len(), 1);
        assert_eq!(hourly[0].uplink_bytes, 80);
        assert_eq!(hourly[0].downlink_bytes, 90);
        assert_eq!(hourly[0].event_count, 2);
        let nic_hourly = database
            .nic_traffic_history("node-history", "eth0", "hour", 0, 7_200)
            .await
            .expect("NIC hour history");
        assert_eq!(nic_hourly.len(), 1);
        assert_eq!((nic_hourly[0].rx_bytes, nic_hourly[0].tx_bytes), (60, 90));
        assert_eq!(
            database
                .subscription_nic_usage("subscription-history")
                .await
                .expect("NIC usage before prune"),
            Some((160, 20_000))
        );

        let pruned = database
            .prune_traffic_history(200_000, 1, 1, 1, 0, 0)
            .await
            .expect("prune history");
        assert_eq!(pruned.xray_events, 2);
        assert_eq!(pruned.interface_snapshots, 2);
        assert_eq!(
            database
                .subscription_xray_usage("subscription-history", 1)
                .await
                .expect("Xray usage after prune"),
            (80, 90)
        );
        assert_eq!(
            database
                .subscription_nic_usage("subscription-history")
                .await
                .expect("NIC usage after prune"),
            Some((160, 20_000))
        );
    }

    #[tokio::test]
    async fn creates_consistent_backup_and_rejects_migration_checksum_tampering() {
        let (temp, database) = test_database().await;
        database
            .ensure_default_admin(100)
            .await
            .expect("default admin");
        let backup_path = temp.path().join("panel-backup.db");
        let verification = database
            .backup_to(&backup_path)
            .await
            .expect("online backup");
        assert_eq!(verification.integrity_messages, ["ok"]);
        assert_eq!(verification.foreign_key_violations, 0);
        assert_eq!(
            verification.schema_version,
            Database::latest_schema_version()
        );
        let backup = Database::connect(&backup_path).await.expect("open backup");
        let admin_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE username = 'admin'")
                .fetch_one(backup.pool())
                .await
                .expect("admin count");
        assert_eq!(admin_count, 1);
        sqlx::query("UPDATE _sqlx_migrations SET checksum = X'00' WHERE version = 1")
            .execute(backup.pool())
            .await
            .expect("tamper migration checksum");
        backup.close().await;
        let error = Database::verify_file(&backup_path)
            .await
            .expect_err("tampered backup");
        assert!(error.to_string().contains("checksum mismatch"));
    }
}
