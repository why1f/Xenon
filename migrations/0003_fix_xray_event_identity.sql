CREATE TABLE xray_traffic_events_v2 (
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
    UNIQUE (agent_id, xray_instance_id, sequence, subscription_id)
);

INSERT OR IGNORE INTO xray_traffic_events_v2
SELECT * FROM xray_traffic_events;

DROP TABLE xray_traffic_events;
ALTER TABLE xray_traffic_events_v2 RENAME TO xray_traffic_events;

CREATE INDEX idx_xray_events_subscription
ON xray_traffic_events(subscription_id, interval_end);
