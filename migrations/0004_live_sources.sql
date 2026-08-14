ALTER TABLE raw_events ADD COLUMN request_id TEXT;
ALTER TABLE raw_events ADD COLUMN store_source_id TEXT;
UPDATE raw_events SET store_source_id = source_id WHERE store_source_id IS NULL;
ALTER TABLE source_epochs ADD COLUMN capability_json TEXT;
ALTER TABLE source_epochs ADD COLUMN capability_hash TEXT;
ALTER TABLE source_epochs ADD COLUMN schema_hash TEXT;

CREATE TABLE pending_requests (
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  thread_key TEXT,
  request_type TEXT NOT NULL,
  state TEXT NOT NULL,
  request_event_seq INTEGER NOT NULL,
  resolved_event_seq INTEGER,
  payload_json TEXT NOT NULL,
  PRIMARY KEY (source_id, epoch_id, request_id)
);

PRAGMA user_version = 4;
