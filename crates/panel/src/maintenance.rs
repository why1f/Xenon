use anyhow::{bail, Context};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;
use xenon_storage::{models::DatabaseVerification, Database};

pub struct DatabaseLock {
    _file: File,
}

#[derive(Debug)]
pub struct BackupOutcome {
    pub path: PathBuf,
    pub checksum: String,
    pub verification: DatabaseVerification,
}

#[derive(Debug)]
pub struct RestoreOutcome {
    pub path: PathBuf,
    pub checksum: String,
    pub verification: DatabaseVerification,
    pub rollback_path: Option<PathBuf>,
}

pub fn acquire_database_lock(database_path: impl AsRef<Path>) -> anyhow::Result<DatabaseLock> {
    let lock_path = suffixed_path(database_path.as_ref(), ".xenon.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create database lock directory {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open database lock {}", lock_path.display()))?;
    FileExt::try_lock_exclusive(&file).with_context(|| {
        format!(
            "database is in use; stop Panel before restore or starting another instance ({})",
            lock_path.display()
        )
    })?;
    Ok(DatabaseLock { _file: file })
}

pub async fn create_backup(
    database: &Database,
    destination: impl AsRef<Path>,
) -> anyhow::Result<BackupOutcome> {
    let destination = destination.as_ref();
    let checksum_path = checksum_path(destination);
    if checksum_path.exists() {
        bail!(
            "backup checksum destination already exists: {}",
            checksum_path.display()
        );
    }
    let verification = database
        .backup_to(destination)
        .await
        .context("create SQLite online backup")?;
    let checksum = sha256_file(destination).await?;
    if let Err(error) = write_checksum_file(destination, &checksum).await {
        let _ = tokio::fs::remove_file(destination).await;
        return Err(error);
    }
    Ok(BackupOutcome {
        path: destination.to_path_buf(),
        checksum,
        verification,
    })
}

pub async fn create_scheduled_backup(
    database: &Database,
    directory: impl AsRef<Path>,
    retain_count: usize,
) -> anyhow::Result<BackupOutcome> {
    let directory = directory.as_ref();
    tokio::fs::create_dir_all(directory)
        .await
        .with_context(|| format!("create backup directory {}", directory.display()))?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let destination = directory.join(format!("panel-{timestamp}.db"));
    let outcome = create_backup(database, &destination).await?;
    prune_scheduled_backups(directory, retain_count).await?;
    Ok(outcome)
}

async fn prune_scheduled_backups(directory: &Path, retain_count: usize) -> anyhow::Result<()> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut backups = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let file_name = path.file_name().and_then(|value| value.to_str());
        if path.extension().and_then(|value| value.to_str()) == Some("db")
            && file_name.is_some_and(|value| value.starts_with("panel-"))
        {
            backups.push(path);
        }
    }
    backups.sort();
    let remove_count = backups.len().saturating_sub(retain_count);
    for path in backups.into_iter().take(remove_count) {
        tokio::fs::remove_file(&path)
            .await
            .with_context(|| format!("remove expired backup {}", path.display()))?;
        let sidecar = checksum_path(&path);
        if sidecar.exists() {
            tokio::fs::remove_file(&sidecar)
                .await
                .with_context(|| format!("remove expired backup checksum {}", sidecar.display()))?;
        }
    }
    Ok(())
}

pub async fn check_database(path: impl AsRef<Path>) -> anyhow::Result<DatabaseVerification> {
    Database::verify_file(path)
        .await
        .context("verify SQLite database")
}

pub async fn restore_database(
    database_path: impl AsRef<Path>,
    backup_path: impl AsRef<Path>,
) -> anyhow::Result<RestoreOutcome> {
    let database_path = database_path.as_ref();
    let backup_path = backup_path.as_ref();
    let _lock = acquire_database_lock(database_path)?;
    if !backup_path.is_file() {
        bail!("backup does not exist: {}", backup_path.display());
    }
    if database_path.exists()
        && std::fs::canonicalize(database_path).ok() == std::fs::canonicalize(backup_path).ok()
    {
        bail!("backup path must differ from the live database path");
    }
    let checksum = verify_checksum_file(backup_path).await?;
    let verification = check_database(backup_path).await?;
    if verification.schema_version > Database::latest_schema_version() {
        bail!(
            "backup schema {} is newer than supported schema {}",
            verification.schema_version,
            Database::latest_schema_version()
        );
    }
    let parent = database_path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("create database directory {}", parent.display()))?;
    let marker = format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp());
    let temporary_path = suffixed_path(database_path, &format!(".restore-{marker}.tmp"));
    if temporary_path.exists() {
        bail!(
            "restore temporary path already exists: {}",
            temporary_path.display()
        );
    }
    tokio::fs::copy(backup_path, &temporary_path)
        .await
        .with_context(|| format!("copy verified backup to {}", temporary_path.display()))?;
    let temporary_file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temporary_path)
        .await?;
    temporary_file.sync_all().await?;
    drop(temporary_file);
    let copied_checksum = sha256_file(&temporary_path).await?;
    if copied_checksum != checksum {
        let _ = tokio::fs::remove_file(&temporary_path).await;
        bail!("restore staging checksum mismatch");
    }
    check_database(&temporary_path)
        .await
        .context("verify staged restore database")?;

    let rollback_path = database_path
        .is_file()
        .then(|| suffixed_path(database_path, &format!(".pre-restore-{marker}")));
    if let Some(rollback) = &rollback_path {
        move_database_group(database_path, rollback)
            .await
            .context("preserve current database for rollback")?;
    }
    if let Err(error) = tokio::fs::rename(&temporary_path, database_path).await {
        if let Some(rollback) = &rollback_path {
            let _ = move_database_group(rollback, database_path).await;
        }
        return Err(error).context("activate restored database");
    }
    if let Err(error) = check_database(database_path).await {
        let failed_path = suffixed_path(database_path, ".failed-restore");
        let _ = tokio::fs::rename(database_path, &failed_path).await;
        if let Some(rollback) = &rollback_path {
            let _ = move_database_group(rollback, database_path).await;
        }
        return Err(error).context("verify activated database");
    }
    Ok(RestoreOutcome {
        path: database_path.to_path_buf(),
        checksum,
        verification,
        rollback_path,
    })
}

async fn move_database_group(source: &Path, destination: &Path) -> anyhow::Result<()> {
    tokio::fs::rename(source, destination)
        .await
        .with_context(|| format!("move {} to {}", source.display(), destination.display()))?;
    let mut moved_sidecars = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = suffixed_path(source, suffix);
        if source_sidecar.exists() {
            let destination_sidecar = suffixed_path(destination, suffix);
            if let Err(error) = tokio::fs::rename(&source_sidecar, &destination_sidecar).await {
                for (moved_source, moved_destination) in moved_sidecars.into_iter().rev() {
                    let _ = tokio::fs::rename(moved_destination, moved_source).await;
                }
                let _ = tokio::fs::rename(destination, source).await;
                return Err(error).with_context(|| {
                    format!(
                        "move SQLite sidecar {} to {}",
                        source_sidecar.display(),
                        destination_sidecar.display()
                    )
                });
            }
            moved_sidecars.push((source_sidecar, destination_sidecar));
        }
    }
    Ok(())
}

async fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {} for checksum", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

async fn write_checksum_file(path: &Path, checksum: &str) -> anyhow::Result<()> {
    let checksum_path = checksum_path(path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("backup.db");
    tokio::fs::write(&checksum_path, format!("{checksum}  {file_name}\n"))
        .await
        .with_context(|| format!("write backup checksum {}", checksum_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&checksum_path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

async fn verify_checksum_file(path: &Path) -> anyhow::Result<String> {
    let sidecar = checksum_path(path);
    let contents = tokio::fs::read_to_string(&sidecar)
        .await
        .with_context(|| format!("read required backup checksum {}", sidecar.display()))?;
    let expected = contents
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("backup checksum file does not start with a SHA-256 value")?
        .to_ascii_lowercase();
    let actual = sha256_file(path).await?;
    if actual != expected {
        bail!("backup SHA-256 mismatch: expected {expected}, got {actual}");
    }
    Ok(actual)
}

fn checksum_path(path: &Path) -> PathBuf {
    suffixed_path(path, ".sha256")
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::{acquire_database_lock, create_backup, restore_database};
    use xenon_storage::Database;

    #[tokio::test]
    async fn restores_verified_backup_and_preserves_previous_database() {
        let temp = tempfile::tempdir().expect("temp directory");
        let source_path = temp.path().join("source.db");
        let backup_path = temp.path().join("backup.db");
        let live_path = temp.path().join("live.db");

        let source = Database::connect(&source_path)
            .await
            .expect("source database");
        source.ensure_default_admin(1).await.expect("source admin");
        sqlx::query("UPDATE users SET display_name = 'Backup state' WHERE username = 'admin'")
            .execute(source.pool())
            .await
            .expect("source marker");
        create_backup(&source, &backup_path).await.expect("backup");
        source.close().await;

        let live = Database::connect(&live_path).await.expect("live database");
        live.ensure_default_admin(1).await.expect("live admin");
        sqlx::query("UPDATE users SET display_name = 'Current state' WHERE username = 'admin'")
            .execute(live.pool())
            .await
            .expect("live marker");
        live.close().await;

        let lock = acquire_database_lock(&live_path).expect("database lock");
        let locked_error = restore_database(&live_path, &backup_path)
            .await
            .expect_err("restore while locked");
        assert!(locked_error.to_string().contains("database is in use"));
        drop(lock);

        let outcome = restore_database(&live_path, &backup_path)
            .await
            .expect("restore database");
        let restored = Database::connect(&live_path)
            .await
            .expect("restored database");
        let restored_marker = sqlx::query_scalar::<_, String>(
            "SELECT display_name FROM users WHERE username = 'admin'",
        )
        .fetch_one(restored.pool())
        .await
        .expect("restored marker");
        assert_eq!(restored_marker, "Backup state");
        restored.close().await;

        let rollback_path = outcome.rollback_path.expect("rollback path");
        let rollback = Database::connect(&rollback_path)
            .await
            .expect("rollback database");
        let rollback_marker = sqlx::query_scalar::<_, String>(
            "SELECT display_name FROM users WHERE username = 'admin'",
        )
        .fetch_one(rollback.pool())
        .await
        .expect("rollback marker");
        assert_eq!(rollback_marker, "Current state");
    }
}
