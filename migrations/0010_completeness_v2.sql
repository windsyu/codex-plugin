BEGIN IMMEDIATE;

-- Coverage JSON is recomputed idempotently by the Rust migration step because
-- retained raw evidence and projection-only databases require typed handling.
PRAGMA user_version = 10;

COMMIT;
