BEGIN IMMEDIATE;

CREATE TABLE thread_goals (
  thread_key TEXT PRIMARY KEY REFERENCES threads(thread_key) ON DELETE CASCADE,
  source_id TEXT NOT NULL,
  source_epoch TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN (
    'active','paused','blocked','usageLimited','budgetLimited','complete'
  )),
  goal_json TEXT NOT NULL,
  updated_event_seq INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX thread_goals_source_epoch
  ON thread_goals(source_id, source_epoch, status, updated_at_ms DESC);

PRAGMA user_version = 15;

COMMIT;
