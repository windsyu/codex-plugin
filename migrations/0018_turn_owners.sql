BEGIN IMMEDIATE;

CREATE TABLE turn_owners (
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  worker_id TEXT NOT NULL REFERENCES session_workers(worker_id),
  codex_thread_id TEXT NOT NULL,
  codex_turn_id TEXT NOT NULL,
  owner_type TEXT NOT NULL CHECK (owner_type IN ('terminal','channel','gateway')),
  owner_id TEXT NOT NULL,
  principal_id TEXT NOT NULL,
  input_lease_id TEXT,
  state TEXT NOT NULL CHECK (state IN (
    'active','completed','interrupted','failed','orphaned','stale'
  )),
  version INTEGER NOT NULL,
  start_command_id TEXT REFERENCES gateway_commands(command_id),
  started_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (source_id, source_epoch, codex_thread_id, codex_turn_id)
);

CREATE INDEX turn_owners_worker_state
  ON turn_owners(worker_id, state, updated_at_ms DESC);

CREATE TABLE turn_owner_transitions (
  transition_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  worker_id TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  codex_turn_id TEXT NOT NULL,
  from_state TEXT,
  to_state TEXT NOT NULL,
  from_version INTEGER,
  to_version INTEGER NOT NULL,
  owner_type TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  principal_id TEXT NOT NULL,
  reason_code TEXT,
  command_id TEXT REFERENCES gateway_commands(command_id),
  occurred_at_ms INTEGER NOT NULL
);

CREATE INDEX turn_owner_transitions_turn
  ON turn_owner_transitions(source_id, source_epoch, codex_thread_id, codex_turn_id, transition_seq);

CREATE TRIGGER turn_owner_transitions_no_update
BEFORE UPDATE ON turn_owner_transitions
BEGIN
  SELECT RAISE(ABORT, 'turn_owner_transitions are append-only');
END;

CREATE TRIGGER turn_owner_transitions_no_delete
BEFORE DELETE ON turn_owner_transitions
BEGIN
  SELECT RAISE(ABORT, 'turn_owner_transitions are append-only');
END;

PRAGMA user_version = 18;

COMMIT;
