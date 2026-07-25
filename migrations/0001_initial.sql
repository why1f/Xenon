CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    landing_host TEXT NOT NULL,
    xray_listen_port INTEGER NOT NULL,
    publish_host TEXT,
    publish_port INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    desired_revision INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (xray_listen_port BETWEEN 1 AND 65535),
    CHECK (publish_port IS NULL OR publish_port BETWEEN 1 AND 65535),
    CHECK ((publish_host IS NULL) = (publish_port IS NULL))
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    xray_uuid TEXT NOT NULL UNIQUE,
    xray_email TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'active',
    starts_at INTEGER NOT NULL,
    expires_at INTEGER,
    traffic_limit_bytes INTEGER,
    traffic_multiplier_basis_points INTEGER NOT NULL DEFAULT 10000,
    reset_policy TEXT NOT NULL DEFAULT 'never',
    reset_anchor INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (traffic_limit_bytes IS NULL OR traffic_limit_bytes > 0),
    CHECK (traffic_multiplier_basis_points > 0),
    CHECK (expires_at IS NULL OR expires_at > starts_at)
);

CREATE TABLE IF NOT EXISTS subscription_nodes (
    subscription_id TEXT NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (subscription_id, node_id)
);

CREATE TABLE IF NOT EXISTS nic_bindings (
    id TEXT PRIMARY KEY NOT NULL,
    subscription_id TEXT NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    interface_name TEXT NOT NULL,
    billing_direction TEXT NOT NULL DEFAULT 'rx_tx',
    traffic_limit_bytes INTEGER NOT NULL,
    initial_used_bytes INTEGER NOT NULL DEFAULT 0,
    reset_policy TEXT NOT NULL DEFAULT 'never',
    reset_anchor INTEGER,
    bound_at INTEGER NOT NULL,
    unbound_at INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    CHECK (traffic_limit_bytes > 0),
    CHECK (initial_used_bytes >= 0)
);

CREATE TABLE IF NOT EXISTS xray_traffic_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id),
    subscription_id TEXT NOT NULL REFERENCES subscriptions(id),
    xray_instance_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    interval_start INTEGER NOT NULL,
    interval_end INTEGER NOT NULL,
    uplink_delta INTEGER NOT NULL,
    downlink_delta INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    UNIQUE (agent_id, xray_instance_id, sequence)
);

CREATE TABLE IF NOT EXISTS interface_snapshots (
    node_id TEXT NOT NULL REFERENCES nodes(id),
    boot_id TEXT NOT NULL,
    interface_name TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    sampled_at INTEGER NOT NULL,
    rx_absolute INTEGER NOT NULL,
    tx_absolute INTEGER NOT NULL,
    PRIMARY KEY (node_id, boot_id, interface_name, sequence)
);

CREATE INDEX IF NOT EXISTS idx_subscriptions_user ON subscriptions(user_id, status);
CREATE INDEX IF NOT EXISTS idx_subscription_nodes_node ON subscription_nodes(node_id, enabled);
CREATE INDEX IF NOT EXISTS idx_xray_events_subscription ON xray_traffic_events(subscription_id, interval_end);
CREATE INDEX IF NOT EXISTS idx_interface_snapshots_lookup ON interface_snapshots(node_id, interface_name, sampled_at);
