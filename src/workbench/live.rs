//! Memory-only reading state and bounded content delivery. No disk or CLI writes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use uuid::Uuid;

use super::capture::CaptureStats;
use super::decode::request::RequestInfo;
use super::decode::{Change, Decoded, DiagnosticCode, ResponseStatus, TextKey};
use super::rollout::patches::NativeFileChange;
use super::rollout::tools::NativeCommand;
use super::rollout::{UserDiagnostic, UserIssue, UserRecord};
mod details;
mod usage;
mod view;
pub use usage::UsageSummary;
use view::{MessageStreamState, ModelAuthor, UserItem};
pub use view::{VIEW_SCHEMA_VERSION, ViewItem};
mod native_patches;
mod native_tools;
pub use details::{DetailError, RequestDetailsPage};
mod tools;
pub use tools::{ExecutionState, ToolCallView, ToolContextView};

#[derive(Clone, Copy)]
pub struct LiveLimits {
    pub item_bytes: usize,
    pub view_bytes: usize,
    pub items: usize,
    pub ring_bytes: usize,
    pub client_bytes: usize,
    pub client_events: usize,
    pub clients: usize,
}
impl Default for LiveLimits {
    fn default() -> Self {
        Self {
            item_bytes: 1024 * 1024,
            view_bytes: 32 * 1024 * 1024,
            items: 512,
            ring_bytes: 16 * 1024 * 1024,
            client_bytes: 2 * 1024 * 1024,
            client_events: 256,
            clients: 32,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveItem {
    pub key: TextKey,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
    pub order_index: u64,
    pub capture_seq: u64,
    pub author: ModelAuthor,
    pub stream_state: MessageStreamState,
    pub finalized: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseView {
    pub request_id: Uuid,
    pub response_id: Option<String>,
    pub status: ResponseStatus,
    /// Retain at most two conflicting reports; never substitute the requested model.
    pub reported_models: Vec<String>,
    pub usage: Option<super::decode::details::ResponseUsage>,
    pub usage_conflict: bool,
    pub observed_duration_ms: Option<f64>,
    #[serde(skip)]
    started_at: Option<Instant>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestView {
    pub request_id: Uuid,
    #[serde(flatten)]
    pub info: RequestInfo,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub request_id: Uuid,
    pub capture_seq: u64,
    pub code: DiagnosticCode,
    #[serde(skip)]
    pub order_index: u64,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub run_epoch: Uuid,
    pub view_seq: u64,
    pub schema_version: u32,
    pub items: Vec<ViewItem>,
    pub tool_contexts: Vec<ToolContextView>,
    pub native_commands: Vec<NativeCommand>,
    pub native_file_changes: Vec<NativeFileChange>,
    pub responses: Vec<ResponseView>,
    pub usage_summary: UsageSummary,
    pub requests: Vec<RequestView>,
    pub user_capture: UserCapture,
    pub diagnostics: Vec<Diagnostic>,
    pub capture: &'static str,
    pub recorder: &'static str,
    pub recorder_status: super::recording::RecorderStatus,
    pub persisted_through_view_seq: u64,
    pub history_coverage: &'static str,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserCapture {
    pub enabled: bool,
    pub diagnostics: Vec<UserDiagnostic>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
enum ViewChange {
    #[serde(rename = "item.patch")]
    Patch {
        item_key: String,
        base_revision: u64,
        revision: u64,
        #[serde(flatten)]
        patch: ItemPatch,
        truncated: bool,
    },
    #[serde(rename = "item.replace")]
    Replace { item: ViewItem },
    #[serde(rename = "tool.context")]
    ToolContext { context: ToolContextView },
    #[serde(rename = "native.command")]
    NativeCommand { command: NativeCommand },
    #[serde(rename = "native.file_change")]
    NativeFileChange { change: NativeFileChange },
    #[serde(rename = "request.state")]
    Response { response: ResponseView },
    #[serde(rename = "request.metadata")]
    Request { request: RequestView },
    #[serde(rename = "user.capture")]
    UserCapture { user_capture: UserCapture },
    #[serde(rename = "capture.gap")]
    Diagnostic { diagnostic: Diagnostic },
}
#[derive(Serialize)]
#[serde(
    tag = "field",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum ItemPatch {
    Text { content_key: String, append: String },
    Arguments { append: String },
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewEvent {
    run_epoch: Uuid,
    view_seq: u64,
    request_id: Option<Uuid>,
    capture_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_summary: Option<UsageSummary>,
    #[serde(flatten)]
    change: ViewChange,
}

pub struct Published {
    pub sequence: u64,
    pub received_at: Instant,
    pub json: String,
}
struct Queued {
    message: Arc<Published>,
    _permit: OwnedSemaphorePermit,
}
struct Subscriber {
    sender: mpsc::Sender<Queued>,
    budget: Arc<Semaphore>,
    stop: watch::Sender<bool>,
}
pub struct Subscription {
    receiver: mpsc::Receiver<Queued>,
    stop: watch::Receiver<bool>,
}
impl Subscription {
    pub async fn recv(&mut self) -> Option<Arc<Published>> {
        if *self.stop.borrow() {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.stop.changed() => None,
            queued = self.receiver.recv() => queued.map(|queued| queued.message),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SubscribeError {
    SnapshotRequired,
    ClientLimit,
}

struct State {
    usage: usage::RunUsage,
    recording_reset: bool,
    recorder: Option<super::recording::Sink>,
    history: Option<super::recording::history::History>,
    details: details::DetailStore,
    sequence: u64,
    snapshot_floor: u64,
    items: VecDeque<LiveItem>,
    text_bytes: usize,
    tools: VecDeque<ToolCallView>,
    tool_bytes: usize,
    tool_contexts: VecDeque<ToolContextView>,
    tool_context_bytes: usize,
    native_commands: VecDeque<NativeCommand>,
    native_command_bytes: usize,
    native_file_changes: VecDeque<NativeFileChange>,
    native_file_change_bytes: usize,
    responses: VecDeque<ResponseView>,
    requests: VecDeque<RequestView>,
    users: VecDeque<UserItem>,
    user_bytes: usize,
    user_capture: UserCapture,
    diagnostics: VecDeque<Diagnostic>,
    partial: bool,
    ring: VecDeque<Arc<Published>>,
    ring_bytes: usize,
    subscribers: Vec<Subscriber>,
    dropped_chunks: u64,
    interrupted_streams: u64,
}
pub struct LiveHub {
    epoch: Uuid,
    limits: LiveLimits,
    state: Mutex<State>,
}

impl LiveHub {
    pub fn new(limits: LiveLimits) -> Arc<Self> {
        assert!(
            limits.item_bytes > 0
                && limits.view_bytes >= limits.item_bytes
                && limits.items > 0
                && limits.ring_bytes > 0
                && limits.client_bytes > 0
                && limits.client_bytes <= u32::MAX as usize
                && limits.client_events > 0
                && limits.clients > 0
        );
        Arc::new(Self {
            epoch: Uuid::new_v4(),
            limits,
            state: Mutex::new(State {
                usage: usage::RunUsage::default(),
                recording_reset: false,
                recorder: None,
                history: None,
                details: details::DetailStore::default(),
                sequence: 0,
                snapshot_floor: 0,
                items: VecDeque::new(),
                text_bytes: 0,
                tools: VecDeque::new(),
                tool_bytes: 0,
                tool_contexts: VecDeque::new(),
                tool_context_bytes: 0,
                native_commands: VecDeque::new(),
                native_command_bytes: 0,
                native_file_changes: VecDeque::new(),
                native_file_change_bytes: 0,
                responses: VecDeque::new(),
                requests: VecDeque::new(),
                users: VecDeque::new(),
                user_bytes: 0,
                user_capture: UserCapture::default(),
                diagnostics: VecDeque::new(),
                partial: false,
                ring: VecDeque::new(),
                ring_bytes: 0,
                subscribers: Vec::new(),
                dropped_chunks: 0,
                interrupted_streams: 0,
            }),
        })
    }
    pub fn epoch(&self) -> Uuid {
        self.epoch
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        self.snapshot_locked(&state)
    }
    fn snapshot_locked(&self, state: &State) -> Snapshot {
        let mut items: Vec<_> = state
            .items
            .iter()
            .cloned()
            .map(ViewItem::Model)
            .chain(
                state
                    .tools
                    .iter()
                    .cloned()
                    .map(|tool| ViewItem::Tool(Box::new(tool))),
            )
            .chain(state.users.iter().cloned().map(ViewItem::User))
            .chain(state.diagnostics.iter().cloned().map(ViewItem::Notice))
            .collect();
        items.sort_by_key(ViewItem::order_index);
        let recorder = state
            .recorder
            .as_ref()
            .map(|r| r.status())
            .unwrap_or_else(|| super::recording::RecorderStatus::disabled(self.epoch));
        Snapshot {
            run_epoch: self.epoch,
            view_seq: state.sequence,
            schema_version: VIEW_SCHEMA_VERSION,
            items,
            tool_contexts: state.tool_contexts.iter().cloned().collect(),
            native_commands: state.native_commands.iter().cloned().collect(),
            native_file_changes: state.native_file_changes.iter().cloned().collect(),
            responses: state.responses.iter().cloned().collect(),
            usage_summary: state.usage.summary(),
            requests: state.requests.iter().cloned().collect(),
            user_capture: state.user_capture.clone(),
            diagnostics: state.diagnostics.iter().cloned().collect(),
            capture: if state.partial { "partial" } else { "ok" },
            recorder: recorder.state,
            persisted_through_view_seq: recorder.persisted_through_view_seq,
            history_coverage: recorder.history_coverage,
            recorder_status: recorder,
        }
    }

    pub fn recorder_status(&self) -> super::recording::RecorderStatus {
        self.state
            .lock()
            .unwrap()
            .recorder
            .as_ref()
            .map(|r| r.status())
            .unwrap_or_else(|| super::recording::RecorderStatus::disabled(self.epoch))
    }
    pub fn recorder_updates(&self) -> Option<watch::Receiver<super::recording::RecorderStatus>> {
        self.state
            .lock()
            .unwrap()
            .recorder
            .as_ref()
            .map(|r| r.subscribe())
    }
    pub(crate) fn attach_recorder(
        &self,
        sink: super::recording::Sink,
    ) -> anyhow::Result<super::recording::replay::Checkpoint> {
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(state.recorder.is_none(), "recorder already attached");
        state.recorder = Some(sink);
        Ok(self.checkpoint_locked(&state))
    }
    fn checkpoint_locked(&self, state: &State) -> super::recording::replay::Checkpoint {
        super::recording::replay::Checkpoint {
            earlier_before: None,
            record_seq: state.recorder.as_ref().map_or(0, |r| r.sequence()),
            snapshot: serde_json::to_value(self.snapshot_locked(state)).expect("safe snapshot DTO"),
            documents: state.details.checkpoint(),
            details_partial: state.details.partial(),
        }
    }
    pub(crate) fn recording_checkpoint(&self) -> super::recording::replay::Checkpoint {
        self.checkpoint_locked(&self.state.lock().unwrap())
    }
    pub(crate) fn set_history(&self, history: super::recording::history::History) {
        self.state.lock().unwrap().history = Some(history);
    }
    pub fn history(&self) -> Option<super::recording::history::History> {
        self.state.lock().unwrap().history.clone()
    }

    pub fn apply(&self, decoded: Decoded) {
        let mut state = self.state.lock().unwrap();
        if let Change::Document { document } = decoded.change {
            if let Some(recorder) = &state.recorder {
                recorder.document(
                    state.sequence,
                    decoded.request_id,
                    decoded.capture_seq,
                    &document,
                );
            }
            state
                .details
                .insert(decoded.request_id, decoded.capture_seq, document);
            return;
        }
        let order_index = state.sequence + 1;
        let refresh_messages = matches!(
            &decoded.change,
            Change::Request { .. } | Change::Response { .. }
        );
        let reconcile = matches!(
            &decoded.change,
            Change::Request { .. } | Change::ToolContext { .. }
        ) || matches!(&decoded.change, Change::Tool { update } if update.operation != super::decode::tool::ToolOperation::Append);
        let change = match decoded.change {
            Change::Tool { update } => {
                let Some(change) = self.apply_tool(&mut state, update, decoded.capture_seq) else {
                    return;
                };
                change
            }
            Change::ToolContext { context } => {
                self.apply_tool_context(&mut state, decoded.request_id, context)
            }
            Change::TextDelta { key, text } => {
                let index = if let Some(index) = state.items.iter().position(|item| item.key == key)
                {
                    index
                } else {
                    let item = LiveItem::new(&state, key.clone(), decoded.capture_seq);
                    state.items.push_back(item);
                    state.items.len() - 1
                };
                let item = &mut state.items[index];
                let append = prefix(
                    text.as_str(),
                    self.limits.item_bytes.saturating_sub(item.text.len()),
                );
                item.truncated |= append.len() < text.as_str().len();
                let base_revision = item.revision;
                if append.is_empty() && base_revision > 0 && !item.truncated {
                    return;
                }
                item.revision += 1;
                item.text.push_str(append);
                let change = if base_revision == 0 {
                    ViewChange::Replace {
                        item: ViewItem::Model(item.clone()),
                    }
                } else {
                    ViewChange::Patch {
                        item_key: view::model_key(&key),
                        base_revision,
                        revision: item.revision,
                        patch: ItemPatch::Text {
                            content_key: key.content_index.to_string(),
                            append: append.to_owned(),
                        },
                        truncated: item.truncated,
                    }
                };
                state.text_bytes += append.len();
                change
            }
            Change::TextReplace { key, text } => {
                let index = if let Some(index) = state.items.iter().position(|item| item.key == key)
                {
                    index
                } else {
                    let item = LiveItem::new(&state, key.clone(), decoded.capture_seq);
                    state.items.push_back(item);
                    state.items.len() - 1
                };
                let item = &mut state.items[index];
                let bounded = prefix(text.as_str(), self.limits.item_bytes);
                let previous_bytes = item.text.len();
                let truncated = bounded.len() < text.as_str().len();
                if item.text == bounded
                    && item.truncated == truncated
                    && item.finalized
                    && item.revision > 0
                {
                    return;
                }
                item.finalized = true;
                item.stream_state = MessageStreamState::Ended;
                item.text = bounded.to_owned();
                item.truncated = truncated;
                item.revision += 1;
                let change = ViewChange::Replace {
                    item: ViewItem::Model(item.clone()),
                };
                state.text_bytes = state.text_bytes - previous_bytes + bounded.len();
                change
            }
            Change::Response {
                response_id,
                status,
                model,
                usage,
            } => {
                let identifiable_start =
                    response_id.is_some() && status == ResponseStatus::Receiving;
                let (usage, usage_conflict) = state.usage.observe(
                    decoded.request_id,
                    response_id.as_deref(),
                    status,
                    usage.as_ref(),
                );
                let mut response = ResponseView {
                    request_id: decoded.request_id,
                    response_id,
                    status,
                    usage,
                    usage_conflict,
                    observed_duration_ms: None,
                    started_at: identifiable_start.then_some(decoded.received_at),
                    reported_models: model
                        .iter()
                        .map(|model| model.as_str().to_owned())
                        .collect(),
                };
                if let Some(existing) = state.responses.iter_mut().find(|existing| {
                    existing.request_id == response.request_id
                        && existing.response_id == response.response_id
                }) {
                    response.started_at = existing.started_at;
                    response.observed_duration_ms = existing.observed_duration_ms.or_else(|| {
                        (status != ResponseStatus::Receiving)
                            .then(|| {
                                existing.started_at.map(|start| {
                                    decoded
                                        .received_at
                                        .saturating_duration_since(start)
                                        .as_secs_f64()
                                        * 1000.0
                                })
                            })
                            .flatten()
                    });
                    response.usage_conflict = response.usage_conflict
                        || existing.usage_conflict
                        || existing
                            .usage
                            .as_ref()
                            .zip(response.usage.as_ref())
                            .is_some_and(|(left, right)| left != right);
                    if existing.usage.is_some() {
                        response.usage = existing.usage.clone();
                    }
                    let mut models = existing.reported_models.clone();
                    for model in &response.reported_models {
                        if models.len() < 2 && !models.contains(model) {
                            models.push(model.clone());
                        }
                    }
                    response.reported_models = models;
                    if existing.status == status
                        && existing.reported_models == response.reported_models
                        && existing.usage == response.usage
                        && existing.usage_conflict == response.usage_conflict
                    {
                        return;
                    }
                    *existing = response.clone();
                } else {
                    state.responses.push_back(response.clone());
                    if state.responses.len() > 256 {
                        state.responses.pop_front();
                        invalidate(&mut state);
                    }
                }
                if response.reported_models.len() > 1
                    || response.usage_conflict
                    || response.usage.as_ref().is_some_and(|usage| usage.invalid)
                {
                    state.partial = true;
                }
                ViewChange::Response { response }
            }
            Change::Request { info } => {
                let request = RequestView {
                    request_id: decoded.request_id,
                    info,
                };
                if let Some(existing) = state.requests.iter_mut().find(|existing| {
                    existing.request_id == request.request_id
                        && existing.info.client_request_index == request.info.client_request_index
                }) {
                    *existing = request.clone();
                } else {
                    state.requests.push_back(request.clone());
                    if state.requests.len() > 256 {
                        state.requests.pop_front();
                        invalidate(&mut state);
                    }
                }
                ViewChange::Request { request }
            }
            Change::Diagnostic { code } => {
                state.usage.diagnostic(code);
                state.partial = true;
                if state
                    .diagnostics
                    .iter()
                    .any(|entry| entry.request_id == decoded.request_id && entry.code == code)
                {
                    return;
                }
                let diagnostic = Diagnostic {
                    request_id: decoded.request_id,
                    capture_seq: decoded.capture_seq,
                    code,
                    order_index,
                };
                state.diagnostics.push_back(diagnostic.clone());
                if state.diagnostics.len() > 128 {
                    state.diagnostics.pop_front();
                    invalidate(&mut state);
                }
                ViewChange::Diagnostic { diagnostic }
            }
            Change::Document { .. } => unreachable!("documents are not live payloads"),
        };
        let notice = match &change {
            ViewChange::Diagnostic { diagnostic } => Some(diagnostic.clone()),
            _ => None,
        };
        self.trim_reading_items(&mut state);
        self.publish(
            &mut state,
            Some(decoded.request_id),
            decoded.capture_seq,
            decoded.received_at,
            change,
        );
        if let Some(diagnostic) = notice {
            self.publish(
                &mut state,
                Some(decoded.request_id),
                decoded.capture_seq,
                decoded.received_at,
                ViewChange::Replace {
                    item: ViewItem::Notice(diagnostic),
                },
            );
        }
        if refresh_messages {
            self.refresh_messages(
                &mut state,
                decoded.request_id,
                decoded.capture_seq,
                decoded.received_at,
            );
        }
        if reconcile {
            self.reconcile_tools(&mut state, decoded.received_at);
        }
    }

    fn publish(
        &self,
        state: &mut State,
        request_id: Option<Uuid>,
        capture_seq: u64,
        received_at: Instant,
        change: ViewChange,
    ) {
        state.sequence += 1;
        let event = ViewEvent {
            run_epoch: self.epoch,
            view_seq: state.sequence,
            request_id,
            capture_seq,
            usage_summary: matches!(
                &change,
                ViewChange::Response { .. } | ViewChange::Diagnostic { .. }
            )
            .then(|| state.usage.summary()),
            change,
        };
        let message = Arc::new(Published {
            sequence: state.sequence,
            received_at,
            json: serde_json::to_string(&event).expect("closed reading DTO is serializable"),
        });
        if let Some(recorder) = &state.recorder {
            let reset = state.recording_reset.then(|| self.checkpoint_locked(state));
            recorder.view(message.clone(), reset);
        }
        state.recording_reset = false;
        state.ring_bytes += message.json.len();
        state.ring.push_back(message.clone());
        while state.ring_bytes > self.limits.ring_bytes {
            let removed = state.ring.pop_front().unwrap();
            state.ring_bytes -= removed.json.len();
            state.snapshot_floor = removed.sequence;
        }
        state.subscribers.retain(|subscriber| {
            let sent = enqueue(subscriber, message.clone());
            if !sent {
                let _ = subscriber.stop.send(true);
            }
            sent
        });
    }

    /// Small closed target snapshot; the reader never clones model text or holds
    /// this lock across filesystem I/O. Metadata is not proof of a user message.
    pub(crate) fn user_targets(&self) -> Vec<(Uuid, String)> {
        let state = self.state.lock().unwrap();
        state
            .requests
            .iter()
            .filter_map(|request| {
                if request.info.purpose != super::decode::request::RequestPurpose::Conversation {
                    return None;
                }
                Some((
                    request.info.codex_thread_id?,
                    request.info.codex_turn_id.clone()?,
                ))
            })
            .collect()
    }

    pub(crate) fn enable_user_reader(&self) {
        let mut state = self.state.lock().unwrap();
        state.user_capture.enabled = true;
        let user_capture = state.user_capture.clone();
        self.publish(
            &mut state,
            None,
            0,
            Instant::now(),
            ViewChange::UserCapture { user_capture },
        );
    }

    pub(crate) fn apply_user(&self, mut user: UserRecord) {
        let mut state = self.state.lock().unwrap();
        if !state.requests.iter().any(|request| {
            request.info.purpose == super::decode::request::RequestPurpose::Conversation
                && request.info.codex_thread_id == Some(user.key.codex_thread_id)
                && request.info.codex_turn_id.as_ref() == Some(&user.key.codex_turn_id)
        }) {
            return;
        }
        if let Some(existing) = state.users.iter().find(|existing| existing.key == user.key) {
            if existing.text != user.text
                || existing.omitted != user.omitted
                || existing.truncated != user.truncated
            {
                drop(state);
                self.user_issue(
                    user.source.source_ref,
                    user.source.byte_offset,
                    UserIssue::IdentityConflict,
                );
            }
            // Same native item replay never duplicates a bubble or overwrites
            // conflicting evidence. Source offsets are not content identity.
            return;
        }
        let bounded = prefix(&user.text, super::rollout::USER_TEXT_BYTES).to_owned();
        user.truncated |= bounded.len() < user.text.len();
        user.text = bounded;
        state.user_bytes += user.text.len();
        let user = UserItem {
            record: user,
            order_index: state.sequence + 1,
        };
        state.users.push_back(user.clone());
        while state.users.len() > 256 || state.user_bytes > 4 * 1024 * 1024 {
            let removed = state.users.pop_front().unwrap();
            state.user_bytes -= removed.text.len();
            state.partial = true;
            invalidate(&mut state);
        }
        self.publish(
            &mut state,
            None,
            0,
            Instant::now(),
            ViewChange::Replace {
                item: ViewItem::User(user),
            },
        );
    }

    pub(crate) fn user_issue(&self, source_ref: Uuid, byte_offset: u64, code: UserIssue) {
        let mut state = self.state.lock().unwrap();
        state.partial = true;
        if state
            .user_capture
            .diagnostics
            .iter()
            .any(|entry| entry.source_ref == source_ref && entry.code == code)
        {
            return;
        }
        state.user_capture.diagnostics.push(UserDiagnostic {
            source_ref,
            byte_offset,
            code,
        });
        if state.user_capture.diagnostics.len() > 32 {
            state.user_capture.diagnostics.remove(0);
        }
        let user_capture = state.user_capture.clone();
        self.publish(
            &mut state,
            None,
            0,
            Instant::now(),
            ViewChange::UserCapture { user_capture },
        );
    }

    pub fn capture_health(&self, stats: CaptureStats) {
        let code = {
            let mut state = self.state.lock().unwrap();
            let loss = stats.dropped_chunks > state.dropped_chunks;
            let interrupted = stats.interrupted_streams > state.interrupted_streams;
            state.dropped_chunks = stats.dropped_chunks;
            state.interrupted_streams = stats.interrupted_streams;
            if loss {
                Some(DiagnosticCode::ObservationGap)
            } else if interrupted {
                Some(DiagnosticCode::Interrupted)
            } else {
                None
            }
        };
        if let Some(code) = code {
            self.apply(Decoded {
                request_id: Uuid::nil(),
                capture_seq: 0,
                received_at: Instant::now(),
                change: Change::Diagnostic { code },
            });
        }
    }

    pub fn subscribe(&self, epoch: Uuid, after: u64) -> Result<Subscription, SubscribeError> {
        let mut state = self.state.lock().unwrap();
        if epoch != self.epoch || after > state.sequence || after < state.snapshot_floor {
            return Err(SubscribeError::SnapshotRequired);
        }
        state
            .subscribers
            .retain(|subscriber| !subscriber.sender.is_closed());
        if state.subscribers.len() >= self.limits.clients {
            return Err(SubscribeError::ClientLimit);
        }
        let (sender, receiver) = mpsc::channel(self.limits.client_events);
        let (stop, stop_rx) = watch::channel(false);
        let subscriber = Subscriber {
            sender,
            budget: Arc::new(Semaphore::new(self.limits.client_bytes)),
            stop,
        };
        for message in state.ring.iter().filter(|message| message.sequence > after) {
            if !enqueue(&subscriber, message.clone()) {
                return Err(SubscribeError::SnapshotRequired);
            }
        }
        state.subscribers.push(subscriber);
        Ok(Subscription {
            receiver,
            stop: stop_rx,
        })
    }
}

pub(crate) fn prefix(text: &str, budget: usize) -> &str {
    let mut end = text.len().min(budget);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn invalidate(state: &mut State) {
    state.recording_reset = true;
    state.snapshot_floor = state.sequence + 1;
    state.ring.clear();
    state.ring_bytes = 0;
    for subscriber in state.subscribers.drain(..) {
        let _ = subscriber.stop.send(true);
    }
}
fn enqueue(subscriber: &Subscriber, message: Arc<Published>) -> bool {
    let Ok(bytes) = u32::try_from(message.json.len()) else {
        return false;
    };
    let Ok(permit) = subscriber.budget.clone().try_acquire_many_owned(bytes) else {
        return false;
    };
    subscriber
        .sender
        .try_send(Queued {
            message,
            _permit: permit,
        })
        .is_ok()
}

#[cfg(test)]
mod tests;
