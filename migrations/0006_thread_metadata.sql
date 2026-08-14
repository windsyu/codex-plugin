BEGIN IMMEDIATE;

ALTER TABLE threads ADD COLUMN parent_thread_id TEXT;
ALTER TABLE threads ADD COLUMN parent_thread_key TEXT;
ALTER TABLE threads ADD COLUMN forked_from_id TEXT;
ALTER TABLE threads ADD COLUMN forked_from_thread_key TEXT;
ALTER TABLE threads ADD COLUMN agent_nickname TEXT;
ALTER TABLE threads ADD COLUMN agent_role TEXT;
ALTER TABLE threads ADD COLUMN agent_path TEXT;
ALTER TABLE threads ADD COLUMN originator TEXT;
ALTER TABLE threads ADD COLUMN cli_version TEXT;
ALTER TABLE threads ADD COLUMN thread_source TEXT;
ALTER TABLE threads ADD COLUMN history_mode TEXT;
ALTER TABLE threads ADD COLUMN history_base_json TEXT;
ALTER TABLE threads ADD COLUMN model_provider TEXT;
ALTER TABLE threads ADD COLUMN reasoning_effort TEXT;
ALTER TABLE threads ADD COLUMN approval_policy TEXT;
ALTER TABLE threads ADD COLUMN approvals_reviewer_json TEXT;
ALTER TABLE threads ADD COLUMN sandbox_json TEXT;
ALTER TABLE threads ADD COLUMN active_permission_profile_json TEXT;
ALTER TABLE threads ADD COLUMN rule_version TEXT NOT NULL DEFAULT 'v1';

CREATE INDEX threads_parent ON threads(parent_thread_key);
CREATE INDEX threads_forked_from ON threads(forked_from_thread_key);

PRAGMA user_version = 6;

COMMIT;
