//! One wire contract for all reading cards. Internal source aggregates are not
//! duplicated: snapshots and replacements serialize the same typed item.
use super::*;
use serde::ser::SerializeMap;

pub const VIEW_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuthor {
    role: &'static str,
    pub requested_model: Option<String>,
    pub reported_models: Vec<String>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStreamState {
    Receiving,
    Ended,
    Incomplete,
}
#[derive(Clone)]
pub struct UserItem {
    pub record: UserRecord,
    pub order_index: u64,
}
impl std::ops::Deref for UserItem {
    type Target = UserRecord;
    fn deref(&self) -> &Self::Target {
        &self.record
    }
}
#[derive(Clone)]
pub enum ViewItem {
    Model(LiveItem),
    User(UserItem),
    Tool(Box<ToolCallView>),
    Notice(Diagnostic),
}

pub(super) fn model_key(key: &TextKey) -> String {
    identity("model", key)
}
pub(super) fn tool_key(key: &TextKey) -> String {
    identity("tool", key)
}
fn identity(kind: &str, key: &impl Serialize) -> String {
    format!(
        "{kind}:{}",
        serde_json::to_string(key).expect("closed identity")
    )
}
fn completeness(truncated: bool, partial: bool, omitted: bool) -> &'static str {
    if truncated || partial {
        "partial"
    } else if omitted {
        "omitted"
    } else {
        "observed"
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Content<'a> {
    content_key: String,
    text: &'a str,
}
#[derive(Serialize)]
#[serde(
    tag = "origin",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Evidence<'a> {
    Model {
        key: &'a TextKey,
        capture_seq: u64,
    },
    Rollout {
        key: &'a super::super::rollout::UserKey,
        source: &'a super::super::rollout::UserSource,
    },
    Diagnostic {
        request_id: Uuid,
        capture_seq: u64,
    },
}

impl ViewItem {
    pub fn order_index(&self) -> u64 {
        match self {
            Self::Model(item) => item.order_index,
            Self::User(item) => item.order_index,
            Self::Tool(item) => item.order_index,
            Self::Notice(item) => item.order_index,
        }
    }
}
impl Serialize for ViewItem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // ToolCallView already has the closed tool payload. Flattening preserves
        // existing result provenance and all execution-conflict information.
        if let Self::Tool(tool) = self {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Tool<'a> {
                item_key: String,
                completeness: &'static str,
                evidence: [Evidence<'a>; 1],
                #[serde(flatten)]
                tool: &'a ToolCallView,
            }
            return Tool {
                item_key: tool_key(&tool.key),
                completeness: completeness(
                    tool.truncated,
                    tool.identity_conflict
                        || tool.result_conflict
                        || tool.arguments_state
                            == super::super::decode::tool::ArgumentState::Incomplete
                        || tool.result.as_ref().is_some_and(|r| r.truncated),
                    tool.result.as_ref().is_some_and(|r| r.omitted),
                ),
                evidence: [Evidence::Model {
                    key: &tool.key,
                    capture_seq: tool.capture_seq,
                }],
                tool,
            }
            .serialize(serializer);
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("orderIndex", &self.order_index())?;
        match self {
            Self::Model(item) => {
                map.serialize_entry("kind", "message")?;
                map.serialize_entry("itemKey", &model_key(&item.key))?;
                map.serialize_entry("revision", &item.revision)?;
                map.serialize_entry("truncated", &item.truncated)?;
                map.serialize_entry(
                    "completeness",
                    completeness(
                        item.truncated,
                        item.stream_state == MessageStreamState::Incomplete,
                        false,
                    ),
                )?;
                map.serialize_entry("author", &item.author)?;
                map.serialize_entry(
                    "content",
                    &[Content {
                        content_key: item.key.content_index.to_string(),
                        text: &item.text,
                    }],
                )?;
                map.serialize_entry("streamState", &item.stream_state)?;
                map.serialize_entry(
                    "evidence",
                    &[Evidence::Model {
                        key: &item.key,
                        capture_seq: item.capture_seq,
                    }],
                )?;
            }
            Self::User(item) => {
                map.serialize_entry("kind", "message")?;
                map.serialize_entry("itemKey", &identity("user", &item.key))?;
                map.serialize_entry("revision", &item.revision)?;
                map.serialize_entry("truncated", &item.truncated)?;
                map.serialize_entry(
                    "completeness",
                    completeness(item.truncated, false, item.omitted),
                )?;
                map.serialize_entry("omitted", &item.omitted)?;
                map.serialize_entry("author", &serde_json::json!({"role":"user"}))?;
                map.serialize_entry(
                    "content",
                    &[Content {
                        content_key: "text".into(),
                        text: &item.text,
                    }],
                )?;
                map.serialize_entry("streamState", &MessageStreamState::Ended)?;
                map.serialize_entry(
                    "evidence",
                    &[Evidence::Rollout {
                        key: &item.key,
                        source: &item.source,
                    }],
                )?;
            }
            Self::Notice(item) => {
                let (code, text) = match item.code {
                    DiagnosticCode::UnknownEvent => (
                        "unknown_type",
                        "未识别的事件；未将其解释为用户、模型或工具。",
                    ),
                    DiagnosticCode::MissingIdentity | DiagnosticCode::ConflictingIdentity => {
                        ("unassigned", "内容归属未确认；请在请求详情中核对来源。")
                    }
                    DiagnosticCode::OmittedByPolicy => ("omitted", "部分内容未纳入当前阅读范围。"),
                    _ => ("capture_gap", "捕获不完整；已有内容仍可阅读。"),
                };
                map.serialize_entry("kind", "notice")?;
                map.serialize_entry(
                    "itemKey",
                    &identity("notice", &(item.request_id, item.code)),
                )?;
                map.serialize_entry("revision", &1)?;
                map.serialize_entry("truncated", &false)?;
                map.serialize_entry("completeness", "partial")?;
                map.serialize_entry("code", code)?;
                map.serialize_entry("text", text)?;
                map.serialize_entry(
                    "evidence",
                    &[Evidence::Diagnostic {
                        request_id: item.request_id,
                        capture_seq: item.capture_seq,
                    }],
                )?;
            }
            Self::Tool(_) => unreachable!(),
        }
        map.end()
    }
}

impl Snapshot {
    pub fn model_items(&self) -> Vec<&LiveItem> {
        self.items
            .iter()
            .filter_map(|item| {
                if let ViewItem::Model(item) = item {
                    Some(item)
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn tools(&self) -> Vec<&ToolCallView> {
        self.items
            .iter()
            .filter_map(|item| {
                if let ViewItem::Tool(item) = item {
                    Some(item.as_ref())
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn user_messages(&self) -> Vec<&UserRecord> {
        self.items
            .iter()
            .filter_map(|item| {
                if let ViewItem::User(item) = item {
                    Some(&item.record)
                } else {
                    None
                }
            })
            .collect()
    }
}

impl LiveItem {
    pub(super) fn new(state: &State, key: TextKey, capture_seq: u64) -> Self {
        let (author, stream_state) = model_metadata(state, &key, false);
        Self {
            key,
            text: String::new(),
            revision: 0,
            truncated: false,
            order_index: state.sequence + 1,
            capture_seq,
            finalized: false,
            author,
            stream_state,
        }
    }
}
fn model_metadata(
    state: &State,
    key: &TextKey,
    finalized: bool,
) -> (ModelAuthor, MessageStreamState) {
    let request = state
        .requests
        .iter()
        .find(|r| r.request_id == key.request_id && r.info.client_request_index.is_none());
    let response = state
        .responses
        .iter()
        .find(|r| r.request_id == key.request_id && r.response_id == key.response_id);
    let author = ModelAuthor {
        role: "assistant",
        requested_model: request.and_then(|r| {
            r.info
                .requested_model
                .as_ref()
                .map(|model| model.as_str().to_owned())
        }),
        reported_models: response
            .map(|r| r.reported_models.clone())
            .unwrap_or_default(),
    };
    let stream_state = if finalized {
        MessageStreamState::Ended
    } else {
        match response.map(|r| r.status) {
            Some(ResponseStatus::Completed) => MessageStreamState::Ended,
            Some(ResponseStatus::Failed | ResponseStatus::Incomplete) => {
                MessageStreamState::Incomplete
            }
            _ => MessageStreamState::Receiving,
        }
    };
    (author, stream_state)
}
impl LiveHub {
    pub(super) fn refresh_messages(
        &self,
        state: &mut State,
        request_id: Uuid,
        capture_seq: u64,
        received_at: Instant,
    ) {
        let mut changes = Vec::new();
        for index in 0..state.items.len() {
            let item = &state.items[index];
            if item.key.request_id != request_id {
                continue;
            }
            let (author, stream_state) = model_metadata(state, &item.key, item.finalized);
            if item.author == author && item.stream_state == stream_state {
                continue;
            }
            let item = &mut state.items[index];
            item.author = author;
            item.stream_state = stream_state;
            item.revision += 1;
            changes.push(item.clone());
        }
        for item in changes {
            self.publish(
                state,
                Some(request_id),
                capture_seq,
                received_at,
                ViewChange::Replace {
                    item: ViewItem::Model(item),
                },
            );
        }
    }
}
