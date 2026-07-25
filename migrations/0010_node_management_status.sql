ALTER TABLE nodes ADD COLUMN management_status TEXT NOT NULL DEFAULT 'active';

UPDATE nodes
SET management_status = status
WHERE status IN ('disabled', 'deleted');

CREATE INDEX IF NOT EXISTS idx_nodes_management_status
ON nodes(management_status, status);
