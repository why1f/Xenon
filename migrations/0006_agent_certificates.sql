CREATE TABLE IF NOT EXISTS agent_certificates (
    fingerprint_sha256 TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL REFERENCES agents(agent_id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER,
    CHECK (expires_at IS NULL OR expires_at > issued_at)
);

CREATE INDEX IF NOT EXISTS idx_agent_certificates_identity
ON agent_certificates(agent_id, node_id, revoked_at, expires_at);
