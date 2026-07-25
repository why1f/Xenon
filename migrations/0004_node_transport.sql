ALTER TABLE nodes ADD COLUMN protocol TEXT NOT NULL DEFAULT 'vless';
ALTER TABLE nodes ADD COLUMN transport TEXT NOT NULL DEFAULT 'tcp';
ALTER TABLE nodes ADD COLUMN security TEXT NOT NULL DEFAULT 'none';
ALTER TABLE nodes ADD COLUMN server_name TEXT;
ALTER TABLE nodes ADD COLUMN reality_public_key TEXT;
ALTER TABLE nodes ADD COLUMN reality_short_id TEXT;
ALTER TABLE nodes ADD COLUMN reality_fingerprint TEXT;

CREATE INDEX IF NOT EXISTS idx_nodes_transport ON nodes(protocol, transport, security);
