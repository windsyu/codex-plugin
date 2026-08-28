BEGIN IMMEDIATE;

-- Codex Desktop creates isolated cwd directories for projectless conversations.
-- They are execution context, not projects shown in the Codex project section.
UPDATE threads
SET project_key = NULL
WHERE (project_key = 'unknown' AND trim(COALESCE(cwd, '')) = '')
   OR (lower(COALESCE(originator, '')) IN ('codex desktop', 'codex_work_desktop')
     AND replace(COALESCE(cwd, ''), '\', '/') GLOB '*/Documents/Codex/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]/*');

PRAGMA user_version = 13;

COMMIT;
