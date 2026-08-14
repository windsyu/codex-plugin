BEGIN IMMEDIATE;

UPDATE raw_events
SET decode_status = 'unknown'
WHERE decode_status = 'decoded'
  AND method GLOB 'rollout/*'
  AND method NOT IN (
    'rollout/session_meta',
    'rollout/response_item',
    'rollout/inter_agent_communication',
    'rollout/inter_agent_communication_metadata',
    'rollout/compacted',
    'rollout/turn_context',
    'rollout/world_state',
    'rollout/event_msg'
  );

PRAGMA user_version = 9;

COMMIT;
