BEGIN IMMEDIATE;

-- FTS5 columns declared UNINDEXED cannot support point lookups. Rebuild the
-- index with each entry sharing the stable rowid of its owning items row so
-- projection updates and purges can delete by the FTS rowid index.
DROP TABLE search_index;

CREATE VIRTUAL TABLE search_index USING fts5(
  entity_key UNINDEXED,
  thread_key UNINDEXED,
  item_id UNINDEXED,
  text,
  tokenize = 'trigram'
);

INSERT INTO search_index(rowid, entity_key, thread_key, item_id, text)
SELECT rowid,
       thread_key || ':' || turn_scope || ':' || item_id,
       thread_key,
       item_id,
       summary_text
FROM items
WHERE summary_text IS NOT NULL AND trim(summary_text) <> '';

PRAGMA user_version = 19;

COMMIT;
