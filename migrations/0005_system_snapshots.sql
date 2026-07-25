CREATE TABLE IF NOT EXISTS system_snapshots (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    sampled_at INTEGER NOT NULL,
    cpu_usage_basis_points INTEGER NOT NULL,
    load_1_milli INTEGER NOT NULL,
    load_5_milli INTEGER NOT NULL,
    load_15_milli INTEGER NOT NULL,
    memory_total_bytes INTEGER NOT NULL,
    memory_used_bytes INTEGER NOT NULL,
    disk_total_bytes INTEGER NOT NULL,
    disk_used_bytes INTEGER NOT NULL,
    PRIMARY KEY (node_id, sequence)
);

CREATE INDEX IF NOT EXISTS idx_system_snapshots_latest
ON system_snapshots(node_id, sampled_at DESC);
