//! Bounded conversion from transient wire observations to safe reading events.
//! Model text and tool generation stay distinct; neither proves a Codex turn end.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::capture::{Direction, Observation, Source, StreamEnd, Transport};
use super::framing::{FrameResult, FramingIssue, MessageKind, SseFramer, WebSocketFramer};
use super::redaction::{RedactionPolicy, SafeText, TextRedactor};

pub mod details;
mod identity;
pub(crate) mod output;
pub mod patch;
pub mod request;
pub mod tool;
pub mod tool_context;
use request::{RequestInfo, Requests};

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextKey {
    pub request_id: Uuid,
    pub response_id: Option<String>,
    pub wire_item_id: String,
    pub content_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    ObservationGap,
    Interrupted,
    IncompleteFrame,
    InvalidJson,
    InvalidWebSocket,
    UnsupportedCompression,
    UnsupportedContent,
    TooLarge,
    Capacity,
    UnknownEvent,
    MissingIdentity,
    ConflictingIdentity,
    OmittedByPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Receiving,
    Completed,
    Failed,
    Incomplete,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    TextDelta {
        key: TextKey,
        text: SafeText,
    },
    TextReplace {
        key: TextKey,
        text: SafeText,
    },
    Response {
        response_id: Option<String>,
        status: ResponseStatus,
        model: Option<SafeText>,
        usage: Option<details::ResponseUsage>,
    },
    Request {
        info: RequestInfo,
    },
    Tool {
        update: tool::ToolUpdate,
    },
    ToolContext {
        context: tool_context::ToolContext,
    },
    Document {
        document: details::DetailDocument,
    },
    Diagnostic {
        code: DiagnosticCode,
    },
}

pub struct Decoded {
    pub request_id: Uuid,
    pub capture_seq: u64,
    pub received_at: Instant,
    pub change: Change,
}

#[derive(Clone, Copy)]
pub struct DecoderLimits {
    pub streams: usize,
    pub frame_bytes: usize,
    pub buffered_bytes: usize,
    pub text_streams: usize,
}

impl Default for DecoderLimits {
    fn default() -> Self {
        Self {
            streams: 128,
            frame_bytes: 1024 * 1024,
            buffered_bytes: 8 * 1024 * 1024,
            text_streams: 256,
        }
    }
}

enum Framer {
    Sse(SseFramer),
    Ws(WebSocketFramer),
    Unavailable,
}

impl Framer {
    fn feed(&mut self, bytes: &[u8], emit: impl FnMut(FrameResult)) {
        match self {
            Self::Sse(inner) => inner.feed(bytes, emit),
            Self::Ws(inner) => inner.feed(bytes, emit),
            Self::Unavailable => {}
        }
    }
    fn gap(&mut self) {
        match self {
            Self::Sse(inner) => inner.gap(),
            Self::Ws(inner) => inner.gap(),
            Self::Unavailable => {}
        }
    }
    fn finish(&mut self) -> Option<FramingIssue> {
        match self {
            Self::Sse(inner) => inner.finish(),
            Self::Ws(inner) => inner.finish(),
            Self::Unavailable => None,
        }
    }
    fn buffered_bytes(&self) -> usize {
        match self {
            Self::Sse(inner) => inner.buffered_bytes(),
            Self::Ws(inner) => inner.buffered_bytes(),
            Self::Unavailable => 0,
        }
    }
}

struct Stream {
    framer: Framer,
    next_sequence: u64,
    last_tick: u64,
    model: ModelDecoder,
}

pub struct Decoder {
    limits: DecoderLimits,
    policy: Arc<RedactionPolicy>,
    streams: HashMap<Uuid, Stream>,
    tick: u64,
    requests: Requests,
}

impl Decoder {
    pub fn new(limits: DecoderLimits, policy: Arc<RedactionPolicy>) -> Self {
        assert!(
            limits.streams > 0
                && limits.frame_bytes > 0
                && limits.buffered_bytes > 0
                && limits.text_streams > 0
        );
        Self {
            limits,
            requests: Requests::new(limits, policy.clone()),
            policy,
            streams: HashMap::new(),
            tick: 0,
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.streams
            .values()
            .map(|stream| stream.framer.buffered_bytes() + stream.model.pending_bytes())
            .sum::<usize>()
            + self.requests.buffered_bytes()
    }

    pub fn push(&mut self, observation: &Observation, mut emit: impl FnMut(Decoded)) {
        if observation.source.direction != Direction::Response {
            let response_bytes = self
                .streams
                .values()
                .map(|stream| stream.framer.buffered_bytes() + stream.model.pending_bytes())
                .sum::<usize>();
            self.requests.push(
                observation,
                self.limits.buffered_bytes.saturating_sub(response_bytes),
                &mut emit,
            );
            return;
        }
        self.tick += 1;
        let id = observation.source.request_id;
        if !self.streams.contains_key(&id) {
            if self.streams.len() >= self.limits.streams {
                let oldest = *self
                    .streams
                    .iter()
                    .min_by_key(|(_, stream)| stream.last_tick)
                    .unwrap()
                    .0;
                self.streams.remove(&oldest);
                emit(Decoded {
                    request_id: oldest,
                    capture_seq: 0,
                    received_at: observation.received_at,
                    change: Change::Diagnostic {
                        code: DiagnosticCode::Capacity,
                    },
                });
            }
            let (framer, issue) = Self::framer(&observation.source, self.limits.frame_bytes);
            if let Some(code) = issue {
                emit_origin(observation, Change::Diagnostic { code }, &mut emit);
            }
            self.streams.insert(
                id,
                Stream {
                    framer,
                    next_sequence: 1,
                    last_tick: self.tick,
                    model: ModelDecoder::new(
                        self.policy.clone(),
                        self.limits.text_streams,
                        observation.source.transport,
                    ),
                },
            );
        }
        let stream = self.streams.get_mut(&id).unwrap();
        stream.last_tick = self.tick;
        if observation.sequence < stream.next_sequence {
            return;
        }
        if observation.sequence != stream.next_sequence {
            stream.framer.gap();
            stream
                .model
                .gap(&mut |change| emit_origin(observation, change, &mut emit));
            emit_origin(
                observation,
                Change::Diagnostic {
                    code: DiagnosticCode::ObservationGap,
                },
                &mut emit,
            );
        }
        stream.next_sequence = observation.sequence + 1;
        let model = &mut stream.model;
        stream.framer.feed(&observation.bytes, |message| {
            model.message(message, &mut |change| {
                emit_origin(observation, change, &mut emit)
            });
        });
        if let Some(end) = observation.end {
            if let Some(issue) = stream.framer.finish() {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: framing_code(issue),
                    },
                    &mut emit,
                );
            }
            stream
                .model
                .finish(&mut |change| emit_origin(observation, change, &mut emit));
            if end == StreamEnd::Interrupted {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: DiagnosticCode::Interrupted,
                    },
                    &mut emit,
                );
            }
            self.streams.remove(&id);
        }
        // A single feed is at most one bounded observation. Any framing buffer
        // that crosses the global budget is discarded with an explicit gap.
        if self.buffered_bytes() > self.limits.buffered_bytes {
            self.streams.remove(&id);
            emit_origin(
                observation,
                Change::Diagnostic {
                    code: DiagnosticCode::Capacity,
                },
                &mut emit,
            );
        }
    }

    fn framer(source: &Source, limit: usize) -> (Framer, Option<DiagnosticCode>) {
        if !source.content_encoding.is_empty()
            && !source.content_encoding.eq_ignore_ascii_case("identity")
        {
            return (
                Framer::Unavailable,
                Some(DiagnosticCode::UnsupportedCompression),
            );
        }
        match source.transport {
            Transport::WebSocket => (Framer::Ws(WebSocketFramer::new(limit, false)), None),
            Transport::Http
                if source
                    .content_type
                    .split(';')
                    .next()
                    .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/event-stream")) =>
            {
                (Framer::Sse(SseFramer::new(limit)), None)
            }
            _ => (
                Framer::Unavailable,
                Some(DiagnosticCode::UnsupportedContent),
            ),
        }
    }
}

fn emit_origin(observation: &Observation, change: Change, emit: &mut impl FnMut(Decoded)) {
    // TextKey request IDs are assigned at this boundary, never taken from JSON.
    let change = match change {
        Change::TextDelta { mut key, text } => {
            key.request_id = observation.source.request_id;
            Change::TextDelta { key, text }
        }
        Change::TextReplace { mut key, text } => {
            key.request_id = observation.source.request_id;
            Change::TextReplace { key, text }
        }
        Change::Tool { mut update } => {
            update.key.request_id = observation.source.request_id;
            Change::Tool { update }
        }
        change => change,
    };
    emit(Decoded {
        request_id: observation.source.request_id,
        capture_seq: observation.sequence,
        received_at: observation.received_at,
        change,
    });
}

fn framing_code(issue: FramingIssue) -> DiagnosticCode {
    match issue {
        FramingIssue::TooLarge => DiagnosticCode::TooLarge,
        FramingIssue::Incomplete => DiagnosticCode::IncompleteFrame,
        FramingIssue::InvalidWebSocket => DiagnosticCode::InvalidWebSocket,
        FramingIssue::UnsupportedExtension => DiagnosticCode::UnsupportedCompression,
        FramingIssue::ObservationGap => DiagnosticCode::ObservationGap,
    }
}

struct ModelDecoder {
    policy: Arc<RedactionPolicy>,
    limit: usize,
    transport: Transport,
    response_id: Option<String>,
    receiving: HashSet<Option<String>>,
    text: HashMap<TextKey, TextRedactor>,
    finalized: HashSet<TextKey>,
    identity_conflict: bool,
    suppress_deltas: bool,
    tools: tool::ToolDecoder,
    identities: identity::ItemIdentities,
}

impl ModelDecoder {
    fn new(policy: Arc<RedactionPolicy>, limit: usize, transport: Transport) -> Self {
        Self {
            identities: identity::ItemIdentities::default(),
            tools: tool::ToolDecoder::new(policy.clone(), limit),
            policy,
            limit,
            transport,
            response_id: None,
            receiving: HashSet::new(),
            text: HashMap::new(),
            finalized: HashSet::new(),
            identity_conflict: false,
            suppress_deltas: false,
        }
    }
    fn pending_bytes(&self) -> usize {
        self.text
            .values()
            .map(TextRedactor::pending_bytes)
            .sum::<usize>()
            + self.tools.pending_bytes()
            + self.identities.bytes()
            + self
                .receiving
                .iter()
                .map(|id| id.as_ref().map_or(0, String::len) + 64)
                .sum::<usize>()
    }
    fn identifier(&self, value: &Value) -> Option<String> {
        let text = value.as_str()?;
        (!text.is_empty()
            && text.len() <= 128
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            && self.policy.scrub(text).as_str() == text)
            .then(|| text.to_owned())
    }
    fn key(
        &mut self,
        item: &Value,
        output_index: &Value,
        content_index: u32,
        response_hint: &Value,
        kind: identity::ContentKind,
    ) -> Result<TextKey, DiagnosticCode> {
        let hinted = self.identifier(response_hint);
        if !response_hint.is_null() && hinted.is_none() {
            return Err(DiagnosticCode::MissingIdentity);
        }
        if self.transport == Transport::Http
            && hinted
                .as_ref()
                .zip(self.response_id.as_ref())
                .is_some_and(|(left, right)| left != right)
        {
            return Err(DiagnosticCode::ConflictingIdentity);
        }
        let response_id = hinted.or_else(|| {
            (self.transport == Transport::Http)
                .then(|| self.response_id.clone())
                .flatten()
        });
        if self.transport == Transport::WebSocket && response_id.is_none() {
            return Err(DiagnosticCode::MissingIdentity);
        }
        let wire = self.identifier(item);
        let index = output_index
            .as_u64()
            .and_then(|index| u32::try_from(index).ok());
        if (!item.is_null() && wire.is_none()) || (!output_index.is_null() && index.is_none()) {
            return Err(DiagnosticCode::MissingIdentity);
        }
        let wire_item_id =
            self.identities
                .resolve(response_id.clone(), wire, index, kind, self.limit)?;
        Ok(TextKey {
            request_id: Uuid::nil(),
            response_id,
            wire_item_id,
            content_index,
        })
    }
    fn room_for(&self, key: &TextKey) -> bool {
        self.text.contains_key(key)
            || self.finalized.contains(key)
            || self.text.len() + self.finalized.len() < self.limit
    }
    fn flush(&mut self, emit: &mut impl FnMut(Change)) {
        self.tools.finish(None, emit);
        for (key, mut redactor) in self.text.drain() {
            let text = redactor.finish();
            if !text.is_empty() {
                emit(Change::TextDelta { key, text });
            }
        }
    }
    fn finish(&mut self, emit: &mut impl FnMut(Change)) {
        self.flush(emit);
        // Transport EOF/cancellation cannot stand in for response.completed.
        // Keep completed WS responses intact when another response is cut off.
        for response_id in self.receiving.drain() {
            emit(Change::Response {
                response_id,
                status: ResponseStatus::Incomplete,
                model: None,
                usage: None,
            });
        }
    }
    fn flush_response(&mut self, response_id: &str, emit: &mut impl FnMut(Change)) {
        self.tools.finish(Some(response_id), emit);
        let keys: Vec<_> = self
            .text
            .keys()
            .filter(|key| key.response_id.as_deref() == Some(response_id))
            .cloned()
            .collect();
        for key in keys {
            let text = self.text.remove(&key).unwrap().finish();
            if !text.is_empty() {
                emit(Change::TextDelta { key, text });
            }
        }
    }
    fn gap(&mut self, emit: &mut impl FnMut(Change)) {
        self.flush(emit);
        // A missing event may contain a credential prefix. Resume only from a
        // full authoritative replacement, never from an unclassified suffix.
        self.suppress_deltas = true;
    }
    fn replace(&mut self, key: TextKey, raw: &str, emit: &mut impl FnMut(Change)) {
        if !self.room_for(&key) {
            emit(Change::Diagnostic {
                code: DiagnosticCode::Capacity,
            });
            return;
        }
        self.text.remove(&key);
        self.finalized.insert(key.clone());
        emit(Change::TextReplace {
            key,
            text: self.policy.scrub(raw),
        });
    }
    fn item_done(
        &mut self,
        item: &Value,
        output_index: &Value,
        response_hint: &Value,
        emit: &mut impl FnMut(Change),
    ) {
        if tool::kind(item).is_some() {
            self.tool_item(item, output_index, response_hint, true, emit);
            return;
        }
        if item["type"] != "message" || item["role"] != "assistant" {
            emit(Change::Diagnostic {
                code: DiagnosticCode::OmittedByPolicy,
            });
            return;
        }
        if let Some(content) = item["content"].as_array() {
            for (index, part) in content.iter().enumerate().take(self.limit) {
                if part["type"] == "output_text" {
                    let key = self.key(
                        &item["id"],
                        output_index,
                        index as u32,
                        response_hint,
                        identity::ContentKind::Text,
                    );
                    match (key, part["text"].as_str()) {
                        (Ok(key), Some(text)) => self.replace(key, text, emit),
                        (Err(code), _) => emit(Change::Diagnostic { code }),
                        (_, None) => emit(Change::Diagnostic {
                            code: DiagnosticCode::UnsupportedContent,
                        }),
                    }
                } else {
                    emit(Change::Diagnostic {
                        code: DiagnosticCode::OmittedByPolicy,
                    });
                }
            }
        }
    }
    fn tool_item(
        &mut self,
        item: &Value,
        output_index: &Value,
        response_hint: &Value,
        complete: bool,
        emit: &mut impl FnMut(Change),
    ) {
        let Some(kind) = tool::kind(item) else { return };
        match self.key(&item["id"], output_index, 0, response_hint, kind.into()) {
            Ok(key) => self.tools.item(key, item, complete, emit),
            Err(code) => emit(Change::Diagnostic { code }),
        }
    }
    fn message(&mut self, message: FrameResult, emit: &mut impl FnMut(Change)) {
        let message = match message {
            Ok(message) => message,
            Err(issue) => {
                self.gap(emit);
                emit(Change::Diagnostic {
                    code: framing_code(issue),
                });
                return;
            }
        };
        if message.kind != MessageKind::Text {
            emit(Change::Diagnostic {
                code: DiagnosticCode::UnsupportedContent,
            });
            return;
        }
        if message.bytes == b"[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&message.bytes) else {
            self.gap(emit);
            emit(Change::Diagnostic {
                code: DiagnosticCode::InvalidJson,
            });
            return;
        };
        let kind = value["type"].as_str().unwrap_or("");
        match kind {
            "response.created" => {
                let response_id = self.identifier(&value["response"]["id"]);
                if self.transport == Transport::Http
                    && self.response_id.is_some()
                    && self.response_id != response_id
                {
                    self.identity_conflict = true;
                    emit(Change::Diagnostic {
                        code: DiagnosticCode::ConflictingIdentity,
                    });
                    return;
                }
                self.response_id = response_id.clone();
                if self.receiving.len() >= self.limit && !self.receiving.contains(&response_id) {
                    emit(Change::Diagnostic {
                        code: DiagnosticCode::Capacity,
                    });
                    return;
                }
                self.receiving.insert(response_id.clone());
                emit(Change::Response {
                    response_id,
                    status: ResponseStatus::Receiving,
                    model: request::safe_name(&value["response"]["model"], &self.policy),
                    usage: None,
                });
            }
            "response.output_text.delta" | "response.output_text.done"
                if !self.identity_conflict =>
            {
                let content_index = value["content_index"]
                    .as_u64()
                    .and_then(|index| u32::try_from(index).ok());
                let key = content_index
                    .ok_or(DiagnosticCode::MissingIdentity)
                    .and_then(|index| {
                        self.key(
                            &value["item_id"],
                            &value["output_index"],
                            index,
                            &value["response_id"],
                            identity::ContentKind::Text,
                        )
                    });
                let key = match key {
                    Ok(key) => key,
                    Err(code) => {
                        emit(Change::Diagnostic { code });
                        return;
                    }
                };
                if !self.room_for(&key) {
                    emit(Change::Diagnostic {
                        code: DiagnosticCode::Capacity,
                    });
                    return;
                }
                if kind.ends_with(".done") {
                    if let Some(text) = value["text"].as_str() {
                        self.replace(key, text, emit);
                    }
                } else if !self.suppress_deltas
                    && !self.finalized.contains(&key)
                    && let Some(delta) = value["delta"].as_str()
                {
                    let text = self
                        .text
                        .entry(key.clone())
                        .or_insert_with(|| TextRedactor::new(self.policy.clone()))
                        .push(delta);
                    if !text.is_empty() {
                        emit(Change::TextDelta { key, text });
                    }
                }
            }
            "response.output_item.added" if !self.identity_conflict => {
                if tool::kind(&value["item"]).is_some() {
                    self.tool_item(
                        &value["item"],
                        &value["output_index"],
                        &value["response_id"],
                        false,
                        emit,
                    );
                } else if value["item"]["type"] == "message"
                    && value["item"]["role"] == "assistant"
                    && let Err(code) = self.key(
                        &value["item"]["id"],
                        &value["output_index"],
                        0,
                        &value["response_id"],
                        identity::ContentKind::Text,
                    )
                {
                    emit(Change::Diagnostic { code });
                }
            }
            "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done"
                if !self.identity_conflict =>
            {
                let complete = kind.ends_with(".done");
                if self.suppress_deltas && !complete {
                    return;
                }
                let tool_kind = if kind.starts_with("response.function") {
                    tool::ToolKind::Function
                } else {
                    tool::ToolKind::Custom
                };
                let field = if complete {
                    if tool_kind == tool::ToolKind::Function {
                        "arguments"
                    } else {
                        "input"
                    }
                } else {
                    "delta"
                };
                match (
                    self.key(
                        &value["item_id"],
                        &value["output_index"],
                        0,
                        &value["response_id"],
                        tool_kind.into(),
                    ),
                    value[field].as_str(),
                ) {
                    (Ok(key), Some(raw)) => self.tools.delta(key, tool_kind, raw, complete, emit),
                    (Err(code), _) => emit(Change::Diagnostic { code }),
                    (_, None) => emit(Change::Diagnostic {
                        code: DiagnosticCode::UnsupportedContent,
                    }),
                }
            }
            "response.output_item.done" if !self.identity_conflict => self.item_done(
                &value["item"],
                &value["output_index"],
                &value["response_id"],
                emit,
            ),
            "response.completed" | "response.failed" | "response.incomplete" => {
                if !self.identity_conflict
                    && let Some(items) = value["response"]["output"].as_array()
                {
                    for (index, item) in items.iter().enumerate().take(self.limit) {
                        self.item_done(item, &Value::from(index), &value["response"]["id"], emit);
                    }
                }
                let status = match kind {
                    "response.completed" => ResponseStatus::Completed,
                    "response.failed" => ResponseStatus::Failed,
                    _ => ResponseStatus::Incomplete,
                };
                // WS events without an explicit ID remain unassigned. Do not
                // associate a completion with whichever response was last seen.
                let response_id = self.identifier(&value["response"]["id"]).or_else(|| {
                    (self.transport == Transport::Http
                        && value["response"]["id"].is_null()
                        && !self.identity_conflict)
                        .then(|| self.response_id.clone())
                        .flatten()
                });
                if !value["response"]["id"].is_null() && response_id.is_none() {
                    emit(Change::Diagnostic {
                        code: DiagnosticCode::MissingIdentity,
                    });
                }
                if self.transport == Transport::Http {
                    self.flush(emit);
                } else if let Some(response_id) = &response_id {
                    self.flush_response(response_id, emit);
                }
                emit(Change::Document {
                    document: details::response(&value["response"], &self.policy),
                });
                // Context must be available before final-state notification.
                // Unidentified WS responses have no shared usage bucket.
                let usage = response_id
                    .as_ref()
                    .and_then(|_| details::usage(&value["response"]["usage"]));
                self.receiving.remove(&response_id);
                emit(Change::Response {
                    response_id,
                    status,
                    model: request::safe_name(&value["response"]["model"], &self.policy),
                    usage,
                });
            }
            "response.in_progress"
            | "response.content_part.added"
            | "response.content_part.done" => {}
            kind if kind.starts_with("response.reasoning")
                || kind.starts_with("response.function_call")
                || kind.starts_with("response.custom_tool_call") =>
            {
                emit(Change::Diagnostic {
                    code: DiagnosticCode::OmittedByPolicy,
                })
            }
            _ => emit(Change::Diagnostic {
                code: DiagnosticCode::UnknownEvent,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
