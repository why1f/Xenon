CREATE TABLE IF NOT EXISTS proxy_nodes (
    id TEXT PRIMARY KEY NOT NULL,
    host_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    protocol TEXT NOT NULL DEFAULT 'vless',
    transport TEXT NOT NULL DEFAULT 'tcp',
    security TEXT NOT NULL DEFAULT 'none',
    listen_port INTEGER NOT NULL,
    publish_host TEXT,
    publish_port INTEGER,
    server_name TEXT,
    websocket_path TEXT,
    vless_encryption TEXT,
    reality_public_key TEXT,
    reality_short_id TEXT,
    reality_fingerprint TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (host_id, name),
    UNIQUE (host_id, listen_port),
    CHECK (listen_port BETWEEN 1 AND 65535),
    CHECK (publish_port IS NULL OR publish_port BETWEEN 1 AND 65535),
    CHECK ((publish_host IS NULL) = (publish_port IS NULL)),
    CHECK (protocol IN ('vless', 'shadowsocks')),
    CHECK (transport IN ('tcp', 'ws')),
    CHECK (security IN ('none', 'tls', 'reality')),
    CHECK (status IN ('active', 'disabled', 'deleted'))
);

CREATE INDEX IF NOT EXISTS idx_proxy_nodes_host
ON proxy_nodes(host_id, status, name);

CREATE TABLE IF NOT EXISTS subscription_proxy_nodes (
    subscription_id TEXT NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    proxy_node_id TEXT NOT NULL REFERENCES proxy_nodes(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (subscription_id, proxy_node_id)
);

CREATE INDEX IF NOT EXISTS idx_subscription_proxy_nodes_node
ON subscription_proxy_nodes(proxy_node_id, enabled);

-- Existing releases modeled one Agent host and one proxy inbound as the same
-- row. Preserve those installations by materializing one protocol node per
-- legacy host and copying the subscription assignments.
INSERT OR IGNORE INTO proxy_nodes (
    id, host_id, name, protocol, transport, security, listen_port,
    publish_host, publish_port, server_name, reality_public_key,
    reality_short_id, reality_fingerprint, status, created_at, updated_at
)
SELECT id, id, name, protocol, transport, security,
       xray_listen_port, publish_host, publish_port, server_name,
       reality_public_key, reality_short_id, reality_fingerprint,
       CASE WHEN management_status = 'active' THEN 'active' ELSE 'disabled' END,
       created_at, updated_at
FROM nodes
WHERE management_status != 'deleted';

INSERT OR IGNORE INTO subscription_proxy_nodes (
    subscription_id, proxy_node_id, enabled, sort_order
)
SELECT sn.subscription_id, pn.id, sn.enabled, sn.sort_order
FROM subscription_nodes sn
INNER JOIN proxy_nodes pn ON pn.host_id = sn.node_id;
