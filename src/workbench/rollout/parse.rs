use super::*;
use crate::workbench::decode::request::safe_name;

pub(super) struct Lines {
    pending: Vec<u8>,
    discarding: bool,
    pub offset: u64,
    start: u64,
    max_line: usize,
}
impl Lines {
    pub fn partial_offset(&self) -> Option<u64> {
        (!self.pending.is_empty() || self.discarding).then_some(self.start)
    }
    pub fn new(max_line: usize) -> Self {
        Self {
            pending: Vec::new(),
            discarding: false,
            offset: 0,
            start: 0,
            max_line,
        }
    }
    pub fn feed(&mut self, bytes: &[u8], mut line: impl FnMut(u64, Result<Value, UserIssue>)) {
        for &byte in bytes {
            self.offset += 1;
            if byte == b'\n' {
                if !self.discarding {
                    line(
                        self.start,
                        serde_json::from_slice(&self.pending).map_err(|_| UserIssue::InvalidLine),
                    );
                }
                self.pending.clear();
                self.discarding = false;
                self.start = self.offset;
            } else if !self.discarding {
                if self.pending.len() == self.max_line {
                    self.pending.clear();
                    self.discarding = true;
                    line(self.start, Err(UserIssue::LineTooLarge));
                } else {
                    self.pending.push(byte);
                }
            }
        }
    }
}

pub(super) fn session(value: &Value, thread: Uuid) -> bool {
    value["type"] == "session_meta"
        && value["payload"]["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            == Some(thread)
        && value["payload"]["thread_source"] == "user"
        && value["payload"]["source"] == "cli"
}

pub(super) fn user(
    value: &Value,
    thread: Uuid,
    source: Uuid,
    offset: u64,
    policy: &Arc<RedactionPolicy>,
) -> Result<Option<UserRecord>, UserIssue> {
    let event = &value["payload"];
    if value["type"] != "event_msg"
        || event["type"] != "item_completed"
        || event["item"]["type"] != "UserMessage"
    {
        return Ok(None);
    }
    if event["thread_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        != Some(thread)
    {
        return Err(UserIssue::IdentityConflict);
    }
    let id = |value: &Value| {
        safe_name(value, policy)
            .filter(|id| !id.as_str().contains(['/', ':']))
            .map(|id| id.as_str().to_owned())
            .ok_or(UserIssue::MissingIdentity)
    };
    let turn = id(&event["turn_id"])?;
    let item = id(&event["item"]["id"])?;
    let content = event["item"]["content"]
        .as_array()
        .ok_or(UserIssue::InvalidLine)?;
    let mut text = String::new();
    let mut omitted = false;
    let mut truncated = false;
    for part in content {
        if part["type"] == "text"
            && let Some(raw) = part["text"].as_str()
        {
            // Scrub the complete part before truncating, including credentials
            // that straddle the preview boundary. Input is bounded by the line cap.
            let safe = policy.scrub(raw);
            if !text.is_empty() {
                text.push('\n');
            }
            let kept = crate::workbench::live::prefix(
                safe.as_str(),
                USER_TEXT_BYTES.saturating_sub(text.len()),
            );
            text.push_str(kept);
            truncated |= kept.len() < safe.as_str().len();
        } else {
            omitted = true;
        }
        if text.len() >= USER_TEXT_BYTES {
            truncated = true;
            break;
        }
    }
    Ok(Some(UserRecord {
        key: UserKey {
            codex_thread_id: thread,
            codex_turn_id: turn,
            native_item_id: item,
        },
        role: "user",
        text,
        revision: 1,
        truncated,
        omitted,
        source: UserSource {
            source_ref: source,
            byte_offset: offset,
            ordinal: value["ordinal"].as_u64(),
        },
    }))
}
