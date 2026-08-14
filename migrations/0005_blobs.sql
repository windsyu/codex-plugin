BEGIN IMMEDIATE;

ALTER TABLE raw_events ADD COLUMN blob_id TEXT;

CREATE TABLE blobs (
  blob_id TEXT PRIMARY KEY,
  stored_hash TEXT NOT NULL UNIQUE,
  media_type TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  relative_path TEXT NOT NULL UNIQUE,
  redaction_json TEXT,
  created_event_seq INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER
);

CREATE TABLE blob_references (
  reference_kind TEXT NOT NULL,
  reference_key TEXT NOT NULL,
  blob_id TEXT NOT NULL,
  event_seq INTEGER NOT NULL,
  PRIMARY KEY (reference_kind, reference_key),
  FOREIGN KEY (blob_id) REFERENCES blobs(blob_id)
);

CREATE INDEX blob_references_blob ON blob_references(blob_id);
CREATE INDEX raw_events_blob ON raw_events(blob_id);

PRAGMA user_version = 5;

COMMIT;
