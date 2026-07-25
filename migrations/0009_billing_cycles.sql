ALTER TABLE subscriptions ADD COLUMN current_cycle_start INTEGER NOT NULL DEFAULT 0;
ALTER TABLE subscriptions ADD COLUMN current_cycle_end INTEGER;

UPDATE subscriptions
SET current_cycle_start = starts_at
WHERE current_cycle_start = 0;

ALTER TABLE nic_bindings ADD COLUMN current_cycle_start INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nic_bindings ADD COLUMN current_cycle_end INTEGER;

UPDATE nic_bindings
SET current_cycle_start = bound_at
WHERE current_cycle_start = 0;

CREATE INDEX IF NOT EXISTS idx_subscriptions_cycle_end
ON subscriptions(current_cycle_end, status);

CREATE INDEX IF NOT EXISTS idx_nic_bindings_cycle_end
ON nic_bindings(current_cycle_end, enabled);
