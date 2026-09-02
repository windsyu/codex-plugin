BEGIN IMMEDIATE;

CREATE TABLE session_workers (
  worker_id TEXT PRIMARY KEY,
  create_command_id TEXT NOT NULL UNIQUE REFERENCES gateway_commands(command_id),
  principal_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  mode TEXT NOT NULL CHECK (mode IN ('new','resume')),
  state TEXT NOT NULL CHECK (state IN (
    'starting','connecting','ready','detached','stopping','exited',
    'stale_epoch','failed','orphaned'
  )),
  version INTEGER NOT NULL,
  input_lease_version INTEGER NOT NULL DEFAULT 0,
  primary_thread_id TEXT,
  canonical_cwd TEXT NOT NULL,
  rows INTEGER NOT NULL,
  cols INTEGER NOT NULL,
  pid INTEGER,
  runtime_dir_name TEXT NOT NULL,
  error_code TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX session_workers_source_state
  ON session_workers(source_id, source_epoch, state, updated_at_ms DESC);

CREATE TABLE session_worker_transitions (
  transition_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  worker_id TEXT NOT NULL REFERENCES session_workers(worker_id),
  from_state TEXT,
  to_state TEXT NOT NULL,
  from_version INTEGER,
  to_version INTEGER NOT NULL,
  reason_code TEXT,
  command_id TEXT REFERENCES gateway_commands(command_id),
  occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX session_worker_transitions_worker
  ON session_worker_transitions(worker_id, transition_seq);

CREATE TRIGGER session_worker_transitions_no_update
BEFORE UPDATE ON session_worker_transitions
BEGIN
  SELECT RAISE(ABORT, 'session_worker_transitions are append-only');
END;

CREATE TRIGGER session_worker_transitions_no_delete
BEFORE DELETE ON session_worker_transitions
BEGIN
  SELECT RAISE(ABORT, 'session_worker_transitions are append-only');
END;

CREATE TABLE thread_leases (
  lease_id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  codex_thread_id TEXT,
  reservation_id TEXT,
  worker_id TEXT NOT NULL REFERENCES session_workers(worker_id),
  role TEXT NOT NULL CHECK (role IN ('primary','side','child')),
  state TEXT NOT NULL CHECK (state IN (
    'acquiring','active','releasing','released','orphaned','stale'
  )),
  version INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  CHECK (
    (codex_thread_id IS NOT NULL AND reservation_id IS NULL)
    OR (codex_thread_id IS NULL AND reservation_id IS NOT NULL)
  )
);

CREATE UNIQUE INDEX thread_leases_active_thread
  ON thread_leases(source_id, source_epoch, codex_thread_id)
  WHERE codex_thread_id IS NOT NULL
    AND state IN ('acquiring','active','releasing','orphaned');

CREATE UNIQUE INDEX thread_leases_active_reservation
  ON thread_leases(source_id, source_epoch, reservation_id)
  WHERE reservation_id IS NOT NULL
    AND state IN ('acquiring','active','releasing','orphaned');

CREATE INDEX thread_leases_worker_state
  ON thread_leases(worker_id, state, updated_at_ms DESC);

CREATE TABLE thread_lease_transitions (
  transition_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  lease_id TEXT NOT NULL REFERENCES thread_leases(lease_id),
  from_state TEXT,
  to_state TEXT NOT NULL,
  from_version INTEGER,
  to_version INTEGER NOT NULL,
  codex_thread_id TEXT,
  reservation_id TEXT,
  reason_code TEXT,
  command_id TEXT REFERENCES gateway_commands(command_id),
  occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX thread_lease_transitions_lease
  ON thread_lease_transitions(lease_id, transition_seq);

CREATE TRIGGER thread_lease_transitions_no_update
BEFORE UPDATE ON thread_lease_transitions
BEGIN
  SELECT RAISE(ABORT, 'thread_lease_transitions are append-only');
END;

CREATE TRIGGER thread_lease_transitions_no_delete
BEFORE DELETE ON thread_lease_transitions
BEGIN
  SELECT RAISE(ABORT, 'thread_lease_transitions are append-only');
END;

CREATE TABLE terminal_attachments (
  attachment_id TEXT PRIMARY KEY,
  worker_id TEXT NOT NULL REFERENCES session_workers(worker_id),
  principal_id TEXT NOT NULL,
  control_token_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN (
    'prepared','connected','detached','expired','closed','orphaned'
  )),
  version INTEGER NOT NULL,
  last_ack INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX terminal_attachments_worker_state
  ON terminal_attachments(worker_id, state, updated_at_ms DESC);

CREATE TABLE input_leases (
  lease_id TEXT PRIMARY KEY,
  worker_id TEXT NOT NULL REFERENCES session_workers(worker_id),
  owner_type TEXT NOT NULL CHECK (owner_type IN ('terminal','channel','gateway')),
  owner_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','releasing','released','expired','stale','orphaned')),
  version INTEGER NOT NULL,
  acquired_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER,
  updated_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX input_leases_active_worker
  ON input_leases(worker_id)
  WHERE state IN ('active','releasing');

CREATE TABLE input_lease_transitions (
  transition_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  lease_id TEXT NOT NULL REFERENCES input_leases(lease_id),
  worker_id TEXT NOT NULL,
  from_state TEXT,
  to_state TEXT NOT NULL,
  from_version INTEGER,
  to_version INTEGER NOT NULL,
  owner_type TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  reason_code TEXT,
  command_id TEXT REFERENCES gateway_commands(command_id),
  occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX input_lease_transitions_lease
  ON input_lease_transitions(lease_id, transition_seq);

CREATE TRIGGER input_lease_transitions_no_update
BEFORE UPDATE ON input_lease_transitions
BEGIN
  SELECT RAISE(ABORT, 'input_lease_transitions are append-only');
END;

CREATE TRIGGER input_lease_transitions_no_delete
BEFORE DELETE ON input_lease_transitions
BEGIN
  SELECT RAISE(ABORT, 'input_lease_transitions are append-only');
END;

PRAGMA user_version = 17;

COMMIT;
