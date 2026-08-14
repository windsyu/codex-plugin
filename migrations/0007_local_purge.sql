BEGIN IMMEDIATE;

CREATE TABLE purged_threads (
  thread_key TEXT PRIMARY KEY,
  store_source_id TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  purged_at_ms INTEGER NOT NULL
);

CREATE TABLE maintenance_audit (
  audit_id TEXT PRIMARY KEY,
  action TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_key TEXT NOT NULL,
  occurred_at_ms INTEGER NOT NULL,
  details_json TEXT NOT NULL
);

CREATE INDEX maintenance_audit_time ON maintenance_audit(occurred_at_ms, audit_id);

PRAGMA user_version = 7;

COMMIT;
