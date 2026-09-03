BEGIN IMMEDIATE;

-- Only App Server -> owner requests are actionable pending interactions.
-- Earlier Session Kernel builds also projected TUI -> App Server client
-- requests (for example thread/read and turn/start), which could never be
-- resolved as approvals, questions, or elicitation requests.
DELETE FROM pending_requests
WHERE EXISTS (
  SELECT 1
  FROM raw_events
  WHERE raw_events.event_seq = pending_requests.request_event_seq
    AND raw_events.protocol_direction = 'tui_to_upstream'
);

PRAGMA user_version = 20;

COMMIT;
