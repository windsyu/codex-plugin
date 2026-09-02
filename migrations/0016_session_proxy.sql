BEGIN IMMEDIATE;

ALTER TABLE raw_events ADD COLUMN protocol_direction TEXT
  CHECK (protocol_direction IS NULL OR protocol_direction IN ('tui_to_upstream','upstream_to_tui'));
ALTER TABLE raw_events ADD COLUMN worker_id TEXT;
ALTER TABLE raw_events ADD COLUMN worker_connection_epoch TEXT;
ALTER TABLE raw_events ADD COLUMN proxy_seq INTEGER;

CREATE UNIQUE INDEX raw_events_worker_connection_seq
  ON raw_events(worker_id, worker_connection_epoch, proxy_seq)
  WHERE worker_id IS NOT NULL AND worker_connection_epoch IS NOT NULL AND proxy_seq IS NOT NULL;

ALTER TABLE gateway_commands ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy_api'
  CHECK (origin IN ('legacy_api','tui','worker_control','channel'));

CREATE TABLE worker_connection_epochs (
  worker_id TEXT NOT NULL,
  connection_epoch TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('connecting','open','closed','failed','outcome_unknown')),
  opened_at_ms INTEGER NOT NULL,
  closed_at_ms INTEGER,
  close_reason TEXT,
  last_proxy_seq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (worker_id, connection_epoch)
);

CREATE INDEX worker_connection_epochs_source
  ON worker_connection_epochs(source_id, source_epoch, state);

PRAGMA user_version = 16;

COMMIT;
