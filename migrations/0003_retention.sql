CREATE TABLE event_dedupes (
  dedupe_key TEXT PRIMARY KEY,
  stored_raw_hash TEXT NOT NULL,
  original_event_seq INTEGER NOT NULL,
  source_id TEXT NOT NULL,
  retained INTEGER NOT NULL DEFAULT 1
);

INSERT INTO event_dedupes(dedupe_key, stored_raw_hash, original_event_seq, source_id, retained)
SELECT dedupe_key, stored_raw_hash, event_seq, source_id, 1
FROM raw_events;

CREATE TABLE retention_state (
  key TEXT PRIMARY KEY,
  value_integer INTEGER,
  updated_at_ms INTEGER NOT NULL
);

INSERT INTO retention_state(key, value_integer, updated_at_ms)
VALUES ('raw_low_watermark', 0, 0);

PRAGMA user_version = 3;
