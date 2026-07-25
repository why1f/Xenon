CREATE TABLE IF NOT EXISTS xray_traffic_aggregates (
    granularity TEXT NOT NULL,
    subscription_id TEXT NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id),
    bucket_start INTEGER NOT NULL,
    bucket_end INTEGER,
    uplink_bytes INTEGER NOT NULL,
    downlink_bytes INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (granularity, subscription_id, node_id, bucket_start),
    CHECK (granularity IN ('hour', 'day', 'cycle')),
    CHECK (uplink_bytes >= 0 AND downlink_bytes >= 0 AND event_count >= 0)
);

CREATE TABLE IF NOT EXISTS nic_traffic_aggregates (
    granularity TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id),
    interface_name TEXT NOT NULL,
    bucket_start INTEGER NOT NULL,
    bucket_end INTEGER NOT NULL,
    rx_bytes INTEGER NOT NULL,
    tx_bytes INTEGER NOT NULL,
    sample_count INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (granularity, node_id, interface_name, bucket_start),
    CHECK (granularity IN ('hour', 'day')),
    CHECK (rx_bytes >= 0 AND tx_bytes >= 0 AND sample_count >= 0)
);

CREATE TABLE IF NOT EXISTS nic_binding_cycle_aggregates (
    binding_id TEXT NOT NULL REFERENCES nic_bindings(id) ON DELETE CASCADE,
    cycle_start INTEGER NOT NULL,
    rx_bytes INTEGER NOT NULL,
    tx_bytes INTEGER NOT NULL,
    sample_count INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (binding_id, cycle_start),
    CHECK (rx_bytes >= 0 AND tx_bytes >= 0 AND sample_count >= 0)
);

CREATE INDEX IF NOT EXISTS idx_xray_aggregates_history
ON xray_traffic_aggregates(subscription_id, granularity, bucket_start);

CREATE INDEX IF NOT EXISTS idx_nic_aggregates_history
ON nic_traffic_aggregates(node_id, interface_name, granularity, bucket_start);

CREATE INDEX IF NOT EXISTS idx_xray_events_retention
ON xray_traffic_events(interval_end);

CREATE INDEX IF NOT EXISTS idx_interface_snapshots_retention
ON interface_snapshots(sampled_at);

INSERT INTO xray_traffic_aggregates
    (granularity, subscription_id, node_id, bucket_start, bucket_end,
     uplink_bytes, downlink_bytes, event_count, updated_at)
SELECT 'hour', subscription_id, node_id,
       (interval_end / 3600) * 3600,
       (interval_end / 3600) * 3600 + 3600,
       SUM(uplink_delta), SUM(downlink_delta), COUNT(*), MAX(received_at)
FROM xray_traffic_events
GROUP BY subscription_id, node_id, (interval_end / 3600) * 3600
ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
    uplink_bytes = excluded.uplink_bytes,
    downlink_bytes = excluded.downlink_bytes,
    event_count = excluded.event_count,
    updated_at = excluded.updated_at;

INSERT INTO xray_traffic_aggregates
    (granularity, subscription_id, node_id, bucket_start, bucket_end,
     uplink_bytes, downlink_bytes, event_count, updated_at)
SELECT 'day', subscription_id, node_id,
       (interval_end / 86400) * 86400,
       (interval_end / 86400) * 86400 + 86400,
       SUM(uplink_delta), SUM(downlink_delta), COUNT(*), MAX(received_at)
FROM xray_traffic_events
GROUP BY subscription_id, node_id, (interval_end / 86400) * 86400
ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
    uplink_bytes = excluded.uplink_bytes,
    downlink_bytes = excluded.downlink_bytes,
    event_count = excluded.event_count,
    updated_at = excluded.updated_at;

INSERT INTO xray_traffic_aggregates
    (granularity, subscription_id, node_id, bucket_start, bucket_end,
     uplink_bytes, downlink_bytes, event_count, updated_at)
SELECT 'cycle', e.subscription_id, e.node_id,
       s.current_cycle_start, s.current_cycle_end,
       SUM(e.uplink_delta), SUM(e.downlink_delta), COUNT(*), MAX(e.received_at)
FROM xray_traffic_events e
INNER JOIN subscriptions s ON s.id = e.subscription_id
WHERE e.interval_end >= s.current_cycle_start
GROUP BY e.subscription_id, e.node_id, s.current_cycle_start, s.current_cycle_end
ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
    bucket_end = excluded.bucket_end,
    uplink_bytes = excluded.uplink_bytes,
    downlink_bytes = excluded.downlink_bytes,
    event_count = excluded.event_count,
    updated_at = excluded.updated_at;

WITH ordered AS (
    SELECT node_id, boot_id, interface_name, sampled_at,
           rx_absolute, tx_absolute,
           LAG(rx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_rx,
           LAG(tx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_tx
    FROM interface_snapshots
), deltas AS (
    SELECT node_id, interface_name, sampled_at,
           CASE WHEN rx_absolute >= previous_rx
                THEN rx_absolute - previous_rx ELSE 0 END AS rx_delta,
           CASE WHEN tx_absolute >= previous_tx
                THEN tx_absolute - previous_tx ELSE 0 END AS tx_delta
    FROM ordered
    WHERE previous_rx IS NOT NULL AND previous_tx IS NOT NULL
)
INSERT INTO nic_traffic_aggregates
    (granularity, node_id, interface_name, bucket_start, bucket_end,
     rx_bytes, tx_bytes, sample_count, updated_at)
SELECT 'hour', node_id, interface_name,
       (sampled_at / 3600) * 3600,
       (sampled_at / 3600) * 3600 + 3600,
       SUM(rx_delta), SUM(tx_delta), COUNT(*), MAX(sampled_at)
FROM deltas
GROUP BY node_id, interface_name, (sampled_at / 3600) * 3600;

WITH ordered AS (
    SELECT node_id, boot_id, interface_name, sampled_at,
           rx_absolute, tx_absolute,
           LAG(rx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_rx,
           LAG(tx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_tx
    FROM interface_snapshots
), deltas AS (
    SELECT node_id, interface_name, sampled_at,
           CASE WHEN rx_absolute >= previous_rx
                THEN rx_absolute - previous_rx ELSE 0 END AS rx_delta,
           CASE WHEN tx_absolute >= previous_tx
                THEN tx_absolute - previous_tx ELSE 0 END AS tx_delta
    FROM ordered
    WHERE previous_rx IS NOT NULL AND previous_tx IS NOT NULL
)
INSERT INTO nic_traffic_aggregates
    (granularity, node_id, interface_name, bucket_start, bucket_end,
     rx_bytes, tx_bytes, sample_count, updated_at)
SELECT 'day', node_id, interface_name,
       (sampled_at / 86400) * 86400,
       (sampled_at / 86400) * 86400 + 86400,
       SUM(rx_delta), SUM(tx_delta), COUNT(*), MAX(sampled_at)
FROM deltas
GROUP BY node_id, interface_name, (sampled_at / 86400) * 86400;

WITH ordered AS (
    SELECT node_id, boot_id, interface_name, sampled_at,
           rx_absolute, tx_absolute,
           LAG(rx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_rx,
           LAG(tx_absolute) OVER (
               PARTITION BY node_id, boot_id, interface_name ORDER BY sequence
           ) AS previous_tx
    FROM interface_snapshots
), deltas AS (
    SELECT node_id, interface_name, sampled_at,
           CASE WHEN rx_absolute >= previous_rx
                THEN rx_absolute - previous_rx ELSE 0 END AS rx_delta,
           CASE WHEN tx_absolute >= previous_tx
                THEN tx_absolute - previous_tx ELSE 0 END AS tx_delta
    FROM ordered
    WHERE previous_rx IS NOT NULL AND previous_tx IS NOT NULL
)
INSERT INTO nic_binding_cycle_aggregates
    (binding_id, cycle_start, rx_bytes, tx_bytes, sample_count, updated_at)
SELECT b.id, b.current_cycle_start,
       SUM(d.rx_delta), SUM(d.tx_delta), COUNT(*), MAX(d.sampled_at)
FROM nic_bindings b
INNER JOIN deltas d
    ON d.node_id = b.node_id AND d.interface_name = b.interface_name
WHERE b.enabled = 1 AND b.unbound_at IS NULL
  AND d.sampled_at > MAX(b.bound_at, b.current_cycle_start)
GROUP BY b.id, b.current_cycle_start;

CREATE TRIGGER IF NOT EXISTS trg_xray_traffic_aggregates
AFTER INSERT ON xray_traffic_events
BEGIN
    INSERT INTO xray_traffic_aggregates
        (granularity, subscription_id, node_id, bucket_start, bucket_end,
         uplink_bytes, downlink_bytes, event_count, updated_at)
    VALUES
        ('hour', NEW.subscription_id, NEW.node_id,
         (NEW.interval_end / 3600) * 3600,
         (NEW.interval_end / 3600) * 3600 + 3600,
         NEW.uplink_delta, NEW.downlink_delta, 1, NEW.received_at)
    ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
        uplink_bytes = uplink_bytes + excluded.uplink_bytes,
        downlink_bytes = downlink_bytes + excluded.downlink_bytes,
        event_count = event_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);

    INSERT INTO xray_traffic_aggregates
        (granularity, subscription_id, node_id, bucket_start, bucket_end,
         uplink_bytes, downlink_bytes, event_count, updated_at)
    VALUES
        ('day', NEW.subscription_id, NEW.node_id,
         (NEW.interval_end / 86400) * 86400,
         (NEW.interval_end / 86400) * 86400 + 86400,
         NEW.uplink_delta, NEW.downlink_delta, 1, NEW.received_at)
    ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
        uplink_bytes = uplink_bytes + excluded.uplink_bytes,
        downlink_bytes = downlink_bytes + excluded.downlink_bytes,
        event_count = event_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);

    INSERT INTO xray_traffic_aggregates
        (granularity, subscription_id, node_id, bucket_start, bucket_end,
         uplink_bytes, downlink_bytes, event_count, updated_at)
    SELECT 'cycle', NEW.subscription_id, NEW.node_id,
           current_cycle_start, current_cycle_end,
           NEW.uplink_delta, NEW.downlink_delta, 1, NEW.received_at
    FROM subscriptions
    WHERE id = NEW.subscription_id AND NEW.interval_end >= current_cycle_start
    ON CONFLICT (granularity, subscription_id, node_id, bucket_start) DO UPDATE SET
        bucket_end = excluded.bucket_end,
        uplink_bytes = uplink_bytes + excluded.uplink_bytes,
        downlink_bytes = downlink_bytes + excluded.downlink_bytes,
        event_count = event_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);
END;

CREATE TRIGGER IF NOT EXISTS trg_nic_traffic_aggregates
AFTER INSERT ON interface_snapshots
WHEN EXISTS (
    SELECT 1 FROM interface_snapshots previous
    WHERE previous.node_id = NEW.node_id
      AND previous.boot_id = NEW.boot_id
      AND previous.interface_name = NEW.interface_name
      AND previous.sequence < NEW.sequence
)
BEGIN
    INSERT INTO nic_traffic_aggregates
        (granularity, node_id, interface_name, bucket_start, bucket_end,
         rx_bytes, tx_bytes, sample_count, updated_at)
    SELECT 'hour', NEW.node_id, NEW.interface_name,
           (NEW.sampled_at / 3600) * 3600,
           (NEW.sampled_at / 3600) * 3600 + 3600,
           CASE WHEN NEW.rx_absolute >= previous.rx_absolute
                THEN NEW.rx_absolute - previous.rx_absolute ELSE 0 END,
           CASE WHEN NEW.tx_absolute >= previous.tx_absolute
                THEN NEW.tx_absolute - previous.tx_absolute ELSE 0 END,
           1, NEW.sampled_at
    FROM interface_snapshots previous
    WHERE previous.node_id = NEW.node_id
      AND previous.boot_id = NEW.boot_id
      AND previous.interface_name = NEW.interface_name
      AND previous.sequence < NEW.sequence
    ORDER BY previous.sequence DESC
    LIMIT 1
    ON CONFLICT (granularity, node_id, interface_name, bucket_start) DO UPDATE SET
        rx_bytes = rx_bytes + excluded.rx_bytes,
        tx_bytes = tx_bytes + excluded.tx_bytes,
        sample_count = sample_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);

    INSERT INTO nic_traffic_aggregates
        (granularity, node_id, interface_name, bucket_start, bucket_end,
         rx_bytes, tx_bytes, sample_count, updated_at)
    SELECT 'day', NEW.node_id, NEW.interface_name,
           (NEW.sampled_at / 86400) * 86400,
           (NEW.sampled_at / 86400) * 86400 + 86400,
           CASE WHEN NEW.rx_absolute >= previous.rx_absolute
                THEN NEW.rx_absolute - previous.rx_absolute ELSE 0 END,
           CASE WHEN NEW.tx_absolute >= previous.tx_absolute
                THEN NEW.tx_absolute - previous.tx_absolute ELSE 0 END,
           1, NEW.sampled_at
    FROM interface_snapshots previous
    WHERE previous.node_id = NEW.node_id
      AND previous.boot_id = NEW.boot_id
      AND previous.interface_name = NEW.interface_name
      AND previous.sequence < NEW.sequence
    ORDER BY previous.sequence DESC
    LIMIT 1
    ON CONFLICT (granularity, node_id, interface_name, bucket_start) DO UPDATE SET
        rx_bytes = rx_bytes + excluded.rx_bytes,
        tx_bytes = tx_bytes + excluded.tx_bytes,
        sample_count = sample_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);

    INSERT INTO nic_binding_cycle_aggregates
        (binding_id, cycle_start, rx_bytes, tx_bytes, sample_count, updated_at)
    SELECT binding.id, binding.current_cycle_start,
           CASE WHEN NEW.rx_absolute >= previous.rx_absolute
                THEN NEW.rx_absolute - previous.rx_absolute ELSE 0 END,
           CASE WHEN NEW.tx_absolute >= previous.tx_absolute
                THEN NEW.tx_absolute - previous.tx_absolute ELSE 0 END,
           1, NEW.sampled_at
    FROM nic_bindings binding
    INNER JOIN interface_snapshots previous
        ON previous.node_id = NEW.node_id
       AND previous.boot_id = NEW.boot_id
       AND previous.interface_name = NEW.interface_name
       AND previous.sequence < NEW.sequence
    WHERE binding.node_id = NEW.node_id
      AND binding.interface_name = NEW.interface_name
      AND binding.enabled = 1 AND binding.unbound_at IS NULL
      AND NEW.sampled_at > MAX(binding.bound_at, binding.current_cycle_start)
      AND previous.sequence = (
          SELECT MAX(candidate.sequence)
          FROM interface_snapshots candidate
          WHERE candidate.node_id = NEW.node_id
            AND candidate.boot_id = NEW.boot_id
            AND candidate.interface_name = NEW.interface_name
            AND candidate.sequence < NEW.sequence
      )
    ON CONFLICT (binding_id, cycle_start) DO UPDATE SET
        rx_bytes = rx_bytes + excluded.rx_bytes,
        tx_bytes = tx_bytes + excluded.tx_bytes,
        sample_count = sample_count + 1,
        updated_at = MAX(updated_at, excluded.updated_at);
END;
