DROP TABLE IF EXISTS search_index;

CREATE VIRTUAL TABLE search_index USING fts5(
  entity_key UNINDEXED,
  thread_key UNINDEXED,
  item_id UNINDEXED,
  text,
  tokenize = 'trigram'
);

INSERT INTO search_index(entity_key, thread_key, item_id, text)
SELECT thread_key || ':' || turn_scope || ':' || item_id,
       thread_key,
       item_id,
       summary_text
FROM items
WHERE summary_text IS NOT NULL AND trim(summary_text) <> '';

PRAGMA user_version = 2;
