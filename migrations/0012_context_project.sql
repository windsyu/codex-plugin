BEGIN IMMEDIATE;

ALTER TABLE threads ADD COLUMN project_key TEXT;
ALTER TABLE threads ADD COLUMN base_instructions_json TEXT;
ALTER TABLE threads ADD COLUMN dynamic_tools_json TEXT;
ALTER TABLE threads ADD COLUMN selected_capability_roots_json TEXT;
ALTER TABLE threads ADD COLUMN memory_mode TEXT;
ALTER TABLE threads ADD COLUMN subagent_history_start_ordinal INTEGER;
ALTER TABLE threads ADD COLUMN multi_agent_version TEXT;
ALTER TABLE threads ADD COLUMN context_window_json TEXT;

CREATE INDEX threads_project_recency ON threads(project_key, COALESCE(recency_at_ms, 0) DESC, thread_key);

PRAGMA user_version = 12;

COMMIT;
