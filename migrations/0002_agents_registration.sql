CREATE TABLE IF NOT EXISTS agents (
    agent_id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL UNIQUE REFERENCES nodes(id) ON DELETE RESTRICT,
    status TEXT NOT NULL DEFAULT 'registered',
    agent_version TEXT NOT NULL,
    xray_version TEXT NOT NULL,
    max_supported_xray_version TEXT NOT NULL,
    boot_id TEXT,
    last_seen_at INTEGER,
    registered_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS registration_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    consumed_agent_id TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status, last_seen_at);
CREATE INDEX IF NOT EXISTS idx_registration_tokens_node ON registration_tokens(node_id, expires_at);
