ALTER TABLE source_epochs ADD COLUMN event_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_epochs ADD COLUMN sequence_gap_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_epochs ADD COLUMN decode_error_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_epochs ADD COLUMN unknown_event_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_epochs ADD COLUMN last_source_seq INTEGER;
ALTER TABLE source_epochs ADD COLUMN last_event_at_ms INTEGER;

CREATE TABLE projection_conflicts (
  conflict_id TEXT PRIMARY KEY,
  thread_key TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  field_name TEXT NOT NULL,
  live_event_seq INTEGER NOT NULL,
  durable_event_seq INTEGER NOT NULL,
  live_value_json TEXT NOT NULL,
  durable_value_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  detected_at_ms INTEGER NOT NULL,
  resolved_at_ms INTEGER,
  UNIQUE(thread_key, entity_type, entity_key, field_name, live_event_seq, durable_event_seq)
);

CREATE INDEX projection_conflicts_thread_status
  ON projection_conflicts(thread_key, status, detected_at_ms DESC);

UPDATE source_epochs SET
  event_count=(SELECT COUNT(*) FROM raw_events e WHERE e.source_id=source_epochs.source_id AND e.epoch_id=source_epochs.epoch_id),
  decode_error_count=(SELECT COUNT(*) FROM raw_events e WHERE e.source_id=source_epochs.source_id AND e.epoch_id=source_epochs.epoch_id AND e.decode_status='error'),
  unknown_event_count=(SELECT COUNT(*) FROM raw_events e WHERE e.source_id=source_epochs.source_id AND e.epoch_id=source_epochs.epoch_id AND e.decode_status='unknown'),
  last_source_seq=(SELECT MAX(source_seq) FROM raw_events e WHERE e.source_id=source_epochs.source_id AND e.epoch_id=source_epochs.epoch_id),
  last_event_at_ms=(SELECT MAX(observed_at_ms) FROM raw_events e WHERE e.source_id=source_epochs.source_id AND e.epoch_id=source_epochs.epoch_id);

PRAGMA user_version = 11;
