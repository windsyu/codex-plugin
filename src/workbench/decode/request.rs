//! Closed, bounded request metadata. Never infer a human message from role:user
//! or a title-generation purpose from prose/output-schema text.
use super::super::framing::WireMessage;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPurpose {
    Conversation,
    Auxiliary,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PurposeBasis {
    CodexTurnMetadata,
    MissingMetadata,
    ConflictingMetadata,
    UnknownMetadata,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestInfo {
    /// HTTP has one request per capture ID. A WS create is not linked to a
    /// response by arrival order; its ordinal remains explicitly unassigned.
    pub client_request_index: Option<u64>,
    pub requested_model: Option<SafeText>,
    pub codex_thread_id: Option<Uuid>,
    pub codex_turn_id: Option<String>,
    pub purpose: RequestPurpose,
    pub purpose_basis: PurposeBasis,
}

pub(crate) fn safe_name(value: &Value, policy: &Arc<RedactionPolicy>) -> Option<SafeText> {
    let text = value.as_str()?;
    if text.is_empty()
        || text.len() > 128
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-/:".contains(&byte))
    {
        return None;
    }
    let safe = policy.scrub(text);
    (safe.as_str() == text).then_some(safe)
}

fn metadata(body: &Value, index: Option<u64>, policy: &Arc<RedactionPolicy>) -> RequestInfo {
    let mut info = RequestInfo {
        client_request_index: index,
        requested_model: safe_name(&body["model"], policy),
        codex_thread_id: None,
        codex_turn_id: None,
        purpose: RequestPurpose::Unknown,
        purpose_basis: PurposeBasis::MissingMetadata,
    };
    let client = &body["client_metadata"];
    let Some(raw) = client["x-codex-turn-metadata"]
        .as_str()
        .filter(|raw| raw.len() <= 32 * 1024)
    else {
        return info;
    };
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        info.purpose_basis = PurposeBasis::UnknownMetadata;
        return info;
    };
    if ["thread_id", "turn_id", "session_id"]
        .iter()
        .any(|key| client.get(key).is_some_and(|flat| flat != &value[key]))
    {
        info.purpose_basis = PurposeBasis::ConflictingMetadata;
        return info;
    }
    info.codex_thread_id = safe_name(&value["thread_id"], policy)
        .and_then(|id| Uuid::parse_str(id.as_str()).ok())
        .filter(|id| !id.is_nil());
    info.codex_turn_id = safe_name(&value["turn_id"], policy)
        .filter(|id| !id.as_str().contains(['/', ':']))
        .map(|id| id.as_str().to_owned());
    info.purpose = match (
        value["request_kind"].as_str(),
        value["thread_source"].as_str(),
    ) {
        (Some("prewarm" | "compaction" | "memory"), _) => RequestPurpose::Auxiliary,
        (
            Some("turn"),
            Some("system" | "thread_title" | "guardian_review" | "memory_consolidation"),
        ) => RequestPurpose::Auxiliary,
        (Some("turn"), Some("user"))
            if info.codex_thread_id.is_some() && info.codex_turn_id.is_some() =>
        {
            RequestPurpose::Conversation
        }
        _ => RequestPurpose::Unknown,
    };
    info.purpose_basis = if info.purpose == RequestPurpose::Unknown {
        PurposeBasis::UnknownMetadata
    } else {
        PurposeBasis::CodexTurnMetadata
    };
    info
}

enum BodyFramer {
    Json(Vec<u8>),
    Ws(WebSocketFramer),
    Unavailable,
}
struct RequestStream {
    framer: BodyFramer,
    sequence: u64,
    tick: u64,
    create_index: u64,
    invalid: bool,
}
pub(super) struct Requests {
    streams: HashMap<Uuid, RequestStream>,
    limits: DecoderLimits,
    policy: Arc<RedactionPolicy>,
    tick: u64,
}
impl Requests {
    pub fn new(limits: DecoderLimits, policy: Arc<RedactionPolicy>) -> Self {
        Self {
            streams: HashMap::new(),
            limits,
            policy,
            tick: 0,
        }
    }
    pub fn buffered_bytes(&self) -> usize {
        self.streams
            .values()
            .map(|stream| match &stream.framer {
                BodyFramer::Json(bytes) => bytes.capacity(),
                BodyFramer::Ws(framer) => framer.buffered_bytes(),
                BodyFramer::Unavailable => 0,
            })
            .sum()
    }
    pub fn push(
        &mut self,
        observation: &Observation,
        budget: usize,
        emit: &mut impl FnMut(Decoded),
    ) {
        let id = observation.source.request_id;
        self.tick += 1;
        if !self.streams.contains_key(&id) {
            if self.streams.len() >= self.limits.streams {
                let oldest = *self
                    .streams
                    .iter()
                    .min_by_key(|(_, stream)| stream.tick)
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
            let source = &observation.source;
            let mut issue = None;
            let framer = if !source.content_encoding.is_empty()
                && !source.content_encoding.eq_ignore_ascii_case("identity")
            {
                issue = Some(DiagnosticCode::UnsupportedCompression);
                BodyFramer::Unavailable
            } else if source.transport == Transport::WebSocket {
                BodyFramer::Ws(WebSocketFramer::new(self.limits.frame_bytes, true))
            } else if source
                .content_type
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
            {
                BodyFramer::Json(Vec::new())
            } else {
                issue = Some(DiagnosticCode::UnsupportedContent);
                BodyFramer::Unavailable
            };
            if let Some(code) = issue {
                emit_origin(observation, Change::Diagnostic { code }, emit);
            }
            self.streams.insert(
                id,
                RequestStream {
                    framer,
                    sequence: 1,
                    tick: self.tick,
                    create_index: 0,
                    invalid: false,
                },
            );
        }
        let stream = self.streams.get_mut(&id).unwrap();
        stream.tick = self.tick;
        if observation.sequence < stream.sequence {
            return;
        }
        if observation.sequence != stream.sequence {
            match &mut stream.framer {
                BodyFramer::Json(bytes) => {
                    bytes.clear();
                    stream.invalid = true;
                }
                BodyFramer::Ws(framer) => framer.gap(),
                _ => {}
            }
            emit_origin(
                observation,
                Change::Diagnostic {
                    code: DiagnosticCode::ObservationGap,
                },
                emit,
            );
        }
        stream.sequence = observation.sequence + 1;
        let policy = &self.policy;
        let create_index = &mut stream.create_index;
        let mut message = |frame: FrameResult| {
            let frame = match frame {
                Ok(frame) => frame,
                Err(issue) => {
                    emit_origin(
                        observation,
                        Change::Diagnostic {
                            code: framing_code(issue),
                        },
                        emit,
                    );
                    return;
                }
            };
            if frame.kind != MessageKind::Text {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: DiagnosticCode::UnsupportedContent,
                    },
                    emit,
                );
                return;
            }
            let Ok(body) = serde_json::from_slice::<Value>(&frame.bytes) else {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: DiagnosticCode::InvalidJson,
                    },
                    emit,
                );
                return;
            };
            let index = if observation.source.transport == Transport::WebSocket {
                if body["type"] != "response.create" {
                    emit_origin(
                        observation,
                        Change::Diagnostic {
                            code: DiagnosticCode::UnknownEvent,
                        },
                        emit,
                    );
                    return;
                }
                *create_index += 1;
                Some(*create_index)
            } else {
                None
            };
            let info = metadata(&body, index, policy);
            if info.purpose_basis == PurposeBasis::ConflictingMetadata {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: DiagnosticCode::ConflictingIdentity,
                    },
                    emit,
                );
            }
            emit_origin(
                observation,
                Change::Document {
                    document: details::request(&body, index, policy),
                },
                emit,
            );
            emit_origin(observation, Change::Request { info }, emit);
            let context = tool_context::extract(&body, index, policy);
            if !context.definitions.is_empty() || !context.outputs.is_empty() || context.partial {
                emit_origin(observation, Change::ToolContext { context }, emit);
            }
        };
        match &mut stream.framer {
            BodyFramer::Json(bytes) if !stream.invalid => {
                if bytes.len().saturating_add(observation.bytes.len())
                    > self.limits.frame_bytes.min(budget)
                {
                    bytes.clear();
                    bytes.shrink_to_fit();
                    stream.invalid = true;
                    message(Err(FramingIssue::TooLarge));
                } else {
                    bytes.reserve_exact(observation.bytes.len());
                    bytes.extend_from_slice(&observation.bytes);
                    if observation.end == Some(StreamEnd::Complete) {
                        message(Ok(WireMessage {
                            kind: MessageKind::Text,
                            bytes: std::mem::take(bytes),
                        }));
                    }
                }
            }
            BodyFramer::Ws(framer) => {
                framer.feed(&observation.bytes, &mut message);
                if observation.end.is_some()
                    && let Some(issue) = framer.finish()
                {
                    message(Err(issue));
                }
            }
            _ => {}
        }
        if observation.end.is_some() {
            if observation.end == Some(StreamEnd::Interrupted) {
                emit_origin(
                    observation,
                    Change::Diagnostic {
                        code: DiagnosticCode::Interrupted,
                    },
                    emit,
                );
            }
            self.streams.remove(&id);
        } else if self.buffered_bytes() > budget {
            self.streams.remove(&id);
            emit_origin(
                observation,
                Change::Diagnostic {
                    code: DiagnosticCode::Capacity,
                },
                emit,
            );
        }
    }
}

#[cfg(test)]
mod tests;
