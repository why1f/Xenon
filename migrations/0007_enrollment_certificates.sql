ALTER TABLE agent_certificates ADD COLUMN certificate_pem TEXT;
ALTER TABLE agent_certificates ADD COLUMN public_key_sha256 TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_certificates_public_key
ON agent_certificates(agent_id, node_id, public_key_sha256);
