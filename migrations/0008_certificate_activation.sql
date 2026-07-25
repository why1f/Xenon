ALTER TABLE agent_certificates ADD COLUMN activated_at INTEGER;

UPDATE agent_certificates
SET activated_at = issued_at
WHERE activated_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_agent_certificates_activation
ON agent_certificates(agent_id, node_id, activated_at, revoked_at);
