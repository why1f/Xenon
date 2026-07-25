use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};
use xenon_protocol::panel_agent::{XrayTrafficBatch, XrayUserTraffic};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredUserTraffic {
    subscription_id: String,
    xray_email: String,
    uplink_delta: u64,
    downlink_delta: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTrafficBatch {
    agent_id: String,
    node_id: String,
    xray_instance_id: String,
    sequence: u64,
    interval_start_unix: u64,
    interval_end_unix: u64,
    users: Vec<StoredUserTraffic>,
}

impl StoredTrafficBatch {
    fn batch_id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.agent_id, self.xray_instance_id, self.sequence
        )
    }

    fn into_proto(self) -> XrayTrafficBatch {
        XrayTrafficBatch {
            agent_id: self.agent_id,
            node_id: self.node_id,
            xray_instance_id: self.xray_instance_id,
            sequence: self.sequence,
            interval_start_unix: self.interval_start_unix,
            interval_end_unix: self.interval_end_unix,
            users: self
                .users
                .into_iter()
                .map(|user| XrayUserTraffic {
                    subscription_id: user.subscription_id,
                    xray_email: user.xray_email,
                    uplink_delta: user.uplink_delta,
                    downlink_delta: user.downlink_delta,
                })
                .collect(),
        }
    }
}

impl From<&XrayTrafficBatch> for StoredTrafficBatch {
    fn from(batch: &XrayTrafficBatch) -> Self {
        Self {
            agent_id: batch.agent_id.clone(),
            node_id: batch.node_id.clone(),
            xray_instance_id: batch.xray_instance_id.clone(),
            sequence: batch.sequence,
            interval_start_unix: batch.interval_start_unix,
            interval_end_unix: batch.interval_end_unix,
            users: batch
                .users
                .iter()
                .map(|user| StoredUserTraffic {
                    subscription_id: user.subscription_id.clone(),
                    xray_email: user.xray_email.clone(),
                    uplink_delta: user.uplink_delta,
                    downlink_delta: user.downlink_delta,
                })
                .collect(),
        }
    }
}

pub struct TrafficSpool {
    path: PathBuf,
    max_batches: usize,
    max_bytes: usize,
    pending: Vec<StoredTrafficBatch>,
}

impl TrafficSpool {
    pub async fn open(
        path: impl Into<PathBuf>,
        max_batches: usize,
        max_bytes: usize,
    ) -> anyhow::Result<Self> {
        let path = path.into();
        let pending = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                if bytes.len() > max_bytes {
                    anyhow::bail!("traffic spool exceeds configured byte limit");
                }
                serde_json::from_slice::<Vec<StoredTrafficBatch>>(&bytes)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        if pending.len() > max_batches {
            anyhow::bail!("traffic spool exceeds configured batch limit");
        }
        let unique = pending
            .iter()
            .map(StoredTrafficBatch::batch_id)
            .collect::<HashSet<_>>();
        if unique.len() != pending.len() {
            anyhow::bail!("traffic spool contains duplicate batch IDs");
        }
        Ok(Self {
            path,
            max_batches,
            max_bytes,
            pending,
        })
    }

    pub fn pending_batches(&self) -> Vec<XrayTrafficBatch> {
        self.pending
            .clone()
            .into_iter()
            .map(StoredTrafficBatch::into_proto)
            .collect()
    }

    pub async fn enqueue(&mut self, batch: &XrayTrafficBatch) -> anyhow::Result<()> {
        let stored = StoredTrafficBatch::from(batch);
        if self
            .pending
            .iter()
            .any(|pending| pending.batch_id() == stored.batch_id())
        {
            return Ok(());
        }
        if self.pending.len() >= self.max_batches {
            anyhow::bail!("traffic spool batch limit reached");
        }
        self.pending.push(stored);
        if let Err(error) = self.persist().await {
            self.pending.pop();
            return Err(error);
        }
        Ok(())
    }

    pub async fn acknowledge(&mut self, batch_id: &str) -> anyhow::Result<()> {
        let previous = self.pending.clone();
        self.pending.retain(|batch| batch.batch_id() != batch_id);
        if self.pending.len() == previous.len() {
            return Ok(());
        }
        if let Err(error) = self.persist().await {
            self.pending = previous;
            return Err(error);
        }
        Ok(())
    }

    async fn persist(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&self.pending)?;
        if bytes.len() > self.max_bytes {
            anyhow::bail!("traffic spool byte limit reached");
        }
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = self.path.with_extension("tmp");
        tokio::fs::write(&temporary, bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).await?;
        }
        #[cfg(windows)]
        if self.path.exists() {
            tokio::fs::remove_file(&self.path).await?;
        }
        tokio::fs::rename(temporary, &self.path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TrafficSpool;
    use xenon_protocol::panel_agent::{XrayTrafficBatch, XrayUserTraffic};

    fn batch(sequence: u64) -> XrayTrafficBatch {
        XrayTrafficBatch {
            agent_id: "agent-a".into(),
            node_id: "node-a".into(),
            xray_instance_id: "instance-a".into(),
            sequence,
            interval_start_unix: 1,
            interval_end_unix: 2,
            users: vec![XrayUserTraffic {
                subscription_id: "subscription-a".into(),
                xray_email: "sub-a@panel".into(),
                uplink_delta: 10,
                downlink_delta: 20,
            }],
        }
    }

    #[tokio::test]
    async fn persists_replays_and_acknowledges_batches() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("spool.json");
        let mut spool = TrafficSpool::open(&path, 4, 64 * 1024)
            .await
            .expect("open spool");
        spool.enqueue(&batch(1)).await.expect("enqueue");
        spool.enqueue(&batch(1)).await.expect("deduplicate");

        let mut reopened = TrafficSpool::open(&path, 4, 64 * 1024)
            .await
            .expect("reopen spool");
        assert_eq!(reopened.pending_batches().len(), 1);
        reopened
            .acknowledge("agent-a:instance-a:1")
            .await
            .expect("acknowledge");
        assert!(reopened.pending_batches().is_empty());
    }
}
