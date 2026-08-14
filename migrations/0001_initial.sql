PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = FULL;
PRAGMA temp_store = MEMORY;

CREATE TABLE IF NOT EXISTS sources (
  source_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  stable_identity TEXT NOT NULL UNIQUE,
  config_json TEXT NOT NULL,
  status TEXT NOT NULL,
  last_seen_at_ms INTEGER,
  last_error_json TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS source_epochs (
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  opened_at_ms INTEGER NOT NULL,
  closed_at_ms INTEGER,
  close_reason TEXT,
  PRIMARY KEY (source_id, epoch_id),
  FOREIGN KEY (source_id) REFERENCES sources(source_id)
);

CREATE TABLE IF NOT EXISTS raw_events (
  event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  source_seq INTEGER NOT NULL,
  dedupe_key TEXT NOT NULL UNIQUE,
  observed_at_ms INTEGER NOT NULL,
  event_at_ms INTEGER,
  thread_key TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  turn_id TEXT,
  item_id TEXT,
  method TEXT NOT NULL,
  phase TEXT NOT NULL,
  durability TEXT NOT NULL DEFAULT 'durable',
  source_fingerprint TEXT NOT NULL,
  stored_raw_hash TEXT NOT NULL,
  raw_json TEXT NOT NULL,
  redaction_json TEXT NOT NULL,
  decode_status TEXT NOT NULL,
  decode_error TEXT,
  FOREIGN KEY (source_id, epoch_id) REFERENCES source_epochs(source_id, epoch_id)
);

CREATE TABLE IF NOT EXISTS source_checkpoints (
  checkpoint_key TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  epoch_id TEXT NOT NULL,
  file_identity TEXT,
  byte_offset INTEGER NOT NULL DEFAULT 0,
  ordinal INTEGER NOT NULL DEFAULT 0,
  current_turn_id TEXT,
  updated_event_seq INTEGER,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS rollout_locations (
  store_source_id TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  path TEXT NOT NULL,
  representation TEXT NOT NULL,
  file_identity TEXT,
  active INTEGER NOT NULL,
  archived INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL,
  PRIMARY KEY (store_source_id, codex_thread_id, path)
);

CREATE TABLE IF NOT EXISTS threads (
  thread_key TEXT PRIMARY KEY,
  store_source_id TEXT NOT NULL,
  codex_thread_id TEXT NOT NULL,
  session_id TEXT,
  name TEXT,
  cwd TEXT,
  source TEXT,
  model TEXT,
  archived INTEGER NOT NULL DEFAULT 0,
  runtime_status TEXT,
  runtime_status_stale INTEGER NOT NULL DEFAULT 1,
  capture_completeness TEXT NOT NULL DEFAULT 'metadata_only',
  completeness_reasons_json TEXT NOT NULL DEFAULT '[]',
  created_at_ms INTEGER,
  updated_at_ms INTEGER,
  recency_at_ms INTEGER,
  last_message_preview TEXT,
  projection_json TEXT NOT NULL DEFAULT '{}',
  provenance_json TEXT NOT NULL DEFAULT '{}',
  last_event_seq INTEGER NOT NULL,
  UNIQUE (store_source_id, codex_thread_id)
);

CREATE TABLE IF NOT EXISTS turns (
  thread_key TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  status TEXT NOT NULL,
  capture_completeness TEXT NOT NULL,
  completeness_reasons_json TEXT NOT NULL,
  coverage_json TEXT NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  execution_context_json TEXT,
  projection_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  last_event_seq INTEGER NOT NULL,
  PRIMARY KEY (thread_key, turn_id),
  FOREIGN KEY (thread_key) REFERENCES threads(thread_key)
);

CREATE TABLE IF NOT EXISTS items (
  thread_key TEXT NOT NULL,
  turn_scope TEXT NOT NULL,
  item_id TEXT NOT NULL,
  turn_id TEXT,
  item_type TEXT NOT NULL,
  status TEXT NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  summary_text TEXT,
  projection_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  last_event_seq INTEGER NOT NULL,
  PRIMARY KEY (thread_key, turn_scope, item_id),
  FOREIGN KEY (thread_key) REFERENCES threads(thread_key)
);

CREATE TABLE IF NOT EXISTS ingest_errors (
  error_id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  epoch_id TEXT,
  checkpoint_key TEXT,
  offset_or_seq INTEGER,
  category TEXT NOT NULL,
  message TEXT NOT NULL,
  preview TEXT,
  first_seen_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL,
  occurrence_count INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
  entity_key UNINDEXED,
  thread_key UNINDEXED,
  item_id UNINDEXED,
  text,
  tokenize = 'unicode61'
);

CREATE INDEX IF NOT EXISTS raw_events_thread_seq ON raw_events(thread_key, event_seq);
CREATE INDEX IF NOT EXISTS raw_events_source_seq ON raw_events(source_id, epoch_id, source_seq);
CREATE INDEX IF NOT EXISTS raw_events_method_seq ON raw_events(method, event_seq);
CREATE INDEX IF NOT EXISTS threads_recency ON threads(recency_at_ms DESC, thread_key);
CREATE INDEX IF NOT EXISTS turns_thread_time ON turns(thread_key, started_at_ms, turn_id);
CREATE INDEX IF NOT EXISTS items_thread_time ON items(thread_key, started_at_ms, item_id);

PRAGMA user_version = 1;
