BEGIN IMMEDIATE;

CREATE TABLE gateway_commands (
  command_id TEXT PRIMARY KEY,
  principal_id TEXT NOT NULL,
  capability TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  thread_key TEXT,
  codex_thread_id TEXT,
  expected_turn_id TEXT,
  expected_request_id TEXT,
  expected_request_version INTEGER,
  input_summary_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN (
    'received','authorized','dispatching','accepted_by_source','running','completed',
    'rejected','failed','cancelled','outcome_unknown'
  )),
  result_summary_json TEXT,
  error_code TEXT,
  error_message TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(principal_id, capability, idempotency_key)
);

CREATE INDEX gateway_commands_thread_state
  ON gateway_commands(thread_key, state, created_at_ms DESC, command_id DESC);
CREATE INDEX gateway_commands_source_epoch
  ON gateway_commands(source_id, source_epoch, state, created_at_ms DESC);

CREATE TABLE command_transitions (
  transition_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  command_id TEXT NOT NULL REFERENCES gateway_commands(command_id),
  from_state TEXT,
  to_state TEXT NOT NULL,
  occurred_at_ms INTEGER NOT NULL,
  reason_code TEXT,
  details_summary_json TEXT NOT NULL
);

CREATE INDEX command_transitions_command
  ON command_transitions(command_id, transition_seq);

CREATE TRIGGER command_transitions_no_update
BEFORE UPDATE ON command_transitions
BEGIN
  SELECT RAISE(ABORT, 'command_transitions are append-only');
END;

CREATE TRIGGER command_transitions_no_delete
BEFORE DELETE ON command_transitions
BEGIN
  SELECT RAISE(ABORT, 'command_transitions are append-only');
END;

CREATE TABLE control_audit (
  audit_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  command_id TEXT NOT NULL REFERENCES gateway_commands(command_id),
  principal_id TEXT NOT NULL,
  capability TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  thread_key TEXT,
  decision TEXT NOT NULL,
  outcome TEXT NOT NULL,
  payload_hash TEXT NOT NULL,
  input_summary_json TEXT NOT NULL,
  occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX control_audit_command ON control_audit(command_id, audit_seq);
CREATE INDEX control_audit_principal_time
  ON control_audit(principal_id, occurred_at_ms DESC, audit_seq DESC);

CREATE TRIGGER control_audit_no_update
BEFORE UPDATE ON control_audit
BEGIN
  SELECT RAISE(ABORT, 'control_audit is append-only');
END;

CREATE TRIGGER control_audit_no_delete
BEFORE DELETE ON control_audit
BEGIN
  SELECT RAISE(ABORT, 'control_audit is append-only');
END;

CREATE TABLE image_uploads (
  upload_id TEXT PRIMARY KEY,
  principal_id TEXT NOT NULL,
  command_id TEXT REFERENCES gateway_commands(command_id),
  mime_type TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  keyed_fingerprint TEXT NOT NULL,
  relative_path TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('staged','attached','deleted','expired')),
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  deleted_at_ms INTEGER
);

ALTER TABLE pending_requests ADD COLUMN request_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE pending_requests ADD COLUMN resolving_command_id TEXT;
ALTER TABLE pending_requests ADD COLUMN resolving_started_at_ms INTEGER;

PRAGMA user_version = 14;

COMMIT;
