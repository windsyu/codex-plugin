BEGIN IMMEDIATE;

CREATE INDEX items_search_lookup ON items(thread_key, item_id);

PRAGMA user_version = 8;

COMMIT;
