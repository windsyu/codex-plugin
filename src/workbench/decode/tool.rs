//! Tool generation facts only. A finished argument stream never proves execution.
use super::*;

pub const PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Function,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolIdentity {
    pub tool_kind: ToolKind,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub namespace: Option<String>,
    #[serde(skip)]
    pub invalid_fields: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentState {
    Receiving,
    Generated,
    Incomplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOperation {
    Begin,
    Append,
    Complete,
    Interrupt,
}

#[derive(Clone, Serialize)]
pub struct ToolUpdate {
    pub key: TextKey,
    pub identity: ToolIdentity,
    pub operation: ToolOperation,
    pub text: SafeText,
    // Raw argument fingerprints stay internal. They are never a public identity
    // or a substitute for the native request domain and explicit call ID.
    #[serde(skip)]
    pub fingerprint: Option<blake3::Hash>,
}

pub fn kind(item: &Value) -> Option<ToolKind> {
    match item["type"].as_str()? {
        "function_call" => Some(ToolKind::Function),
        "custom_tool_call" => Some(ToolKind::Custom),
        _ => None,
    }
}
pub fn identifier(value: &Value, policy: &Arc<RedactionPolicy>) -> Option<String> {
    request::safe_name(value, policy).map(|value| value.as_str().to_owned())
}
pub fn identity(item: &Value, tool_kind: ToolKind, policy: &Arc<RedactionPolicy>) -> ToolIdentity {
    let call_id = identifier(&item["call_id"], policy);
    let name = identifier(&item["name"], policy);
    let namespace = identifier(&item["namespace"], policy);
    let invalid_fields = [
        ("call_id", &call_id),
        ("name", &name),
        ("namespace", &namespace),
    ]
    .iter()
    .any(|(field, parsed)| {
        item.get(*field).is_some_and(|value| !value.is_null()) && parsed.is_none()
    });
    ToolIdentity {
        tool_kind,
        call_id,
        name,
        namespace,
        invalid_fields,
    }
}
pub fn arguments(item: &Value, kind: ToolKind) -> Option<&str> {
    item[if kind == ToolKind::Function {
        "arguments"
    } else {
        "input"
    }]
    .as_str()
}
pub fn fingerprint(identity: &ToolIdentity, raw: &str) -> Option<blake3::Hash> {
    if identity.invalid_fields {
        return None;
    }
    identity.call_id.as_ref()?;
    identity.name.as_ref()?;
    // Tuple encoding prevents delimiter collisions; no raw content is retained.
    Some(blake3::hash(&serde_json::to_vec(&(identity, raw)).ok()?))
}

struct Generating {
    identity: ToolIdentity,
    redactor: Option<TextRedactor>,
    finalized: bool,
}
pub(super) struct ToolDecoder {
    policy: Arc<RedactionPolicy>,
    streams: HashMap<TextKey, Generating>,
    limit: usize,
}
impl ToolDecoder {
    pub fn new(policy: Arc<RedactionPolicy>, limit: usize) -> Self {
        Self {
            policy,
            streams: HashMap::new(),
            limit,
        }
    }
    pub fn pending_bytes(&self) -> usize {
        self.streams
            .values()
            .filter_map(|stream| stream.redactor.as_ref())
            .map(TextRedactor::pending_bytes)
            .sum()
    }
    fn update(
        &self,
        key: TextKey,
        identity: ToolIdentity,
        operation: ToolOperation,
        text: SafeText,
        fingerprint: Option<blake3::Hash>,
        emit: &mut impl FnMut(Change),
    ) {
        emit(Change::Tool {
            update: ToolUpdate {
                key,
                identity,
                operation,
                text,
                fingerprint,
            },
        });
    }
    pub fn item(
        &mut self,
        key: TextKey,
        item: &Value,
        complete: bool,
        emit: &mut impl FnMut(Change),
    ) {
        let Some(kind) = kind(item) else {
            return;
        };
        let identity = identity(item, kind, &self.policy);
        if identity.invalid_fields || identity.name.is_none() || identity.call_id.is_none() {
            emit(Change::Diagnostic {
                code: DiagnosticCode::MissingIdentity,
            });
        }
        if !self.streams.contains_key(&key) && self.streams.len() >= self.limit {
            emit(Change::Diagnostic {
                code: DiagnosticCode::Capacity,
            });
            return;
        }
        if complete {
            if let Some(raw) = arguments(item, kind) {
                self.streams.insert(
                    key.clone(),
                    Generating {
                        identity: identity.clone(),
                        redactor: None,
                        finalized: true,
                    },
                );
                self.update(
                    key,
                    identity.clone(),
                    ToolOperation::Complete,
                    self.policy.scrub_tool(raw),
                    fingerprint(&identity, raw),
                    emit,
                );
            } else {
                self.update(
                    key,
                    identity,
                    ToolOperation::Interrupt,
                    self.policy.scrub_tool(""),
                    None,
                    emit,
                );
                emit(Change::Diagnostic {
                    code: DiagnosticCode::UnsupportedContent,
                });
            }
        } else {
            // Repeated item.added must not replay the initial arguments.
            if self.streams.contains_key(&key) {
                self.update(
                    key,
                    identity,
                    ToolOperation::Begin,
                    self.policy.scrub_tool(""),
                    None,
                    emit,
                );
                return;
            }
            let mut redactor = TextRedactor::tool(self.policy.clone());
            let text = redactor.push(arguments(item, kind).unwrap_or(""));
            self.update(
                key.clone(),
                identity.clone(),
                ToolOperation::Begin,
                text,
                None,
                emit,
            );
            self.streams.insert(
                key,
                Generating {
                    identity,
                    redactor: Some(redactor),
                    finalized: false,
                },
            );
        }
    }
    pub fn delta(
        &mut self,
        key: TextKey,
        kind: ToolKind,
        raw: &str,
        complete: bool,
        emit: &mut impl FnMut(Change),
    ) {
        if !self.streams.contains_key(&key) && self.streams.len() >= self.limit {
            emit(Change::Diagnostic {
                code: DiagnosticCode::Capacity,
            });
            return;
        }
        let stream = self
            .streams
            .entry(key.clone())
            .or_insert_with(|| Generating {
                identity: ToolIdentity {
                    tool_kind: kind,
                    call_id: None,
                    name: None,
                    namespace: None,
                    invalid_fields: false,
                },
                redactor: Some(TextRedactor::tool(self.policy.clone())),
                finalized: false,
            });
        if stream.identity.tool_kind != kind {
            emit(Change::Diagnostic {
                code: DiagnosticCode::ConflictingIdentity,
            });
            return;
        }
        if stream.finalized {
            return;
        }
        let identity = stream.identity.clone();
        let (text, operation, fingerprint) = if complete {
            stream.finalized = true;
            stream.redactor = None;
            (
                self.policy.scrub_tool(raw),
                ToolOperation::Complete,
                fingerprint(&identity, raw),
            )
        } else {
            let Some(redactor) = &mut stream.redactor else {
                return;
            };
            (redactor.push(raw), ToolOperation::Append, None)
        };
        if !text.is_empty() || complete {
            self.update(key, identity, operation, text, fingerprint, emit);
        }
    }
    pub fn finish(&mut self, response: Option<&str>, emit: &mut impl FnMut(Change)) {
        for (key, stream) in &mut self.streams {
            if stream.finalized || response.is_some_and(|id| key.response_id.as_deref() != Some(id))
            {
                continue;
            }
            if let Some(mut redactor) = stream.redactor.take() {
                let text = redactor.finish();
                if !text.is_empty() {
                    emit(Change::Tool {
                        update: ToolUpdate {
                            key: key.clone(),
                            identity: stream.identity.clone(),
                            operation: ToolOperation::Append,
                            text,
                            fingerprint: None,
                        },
                    });
                }
                emit(Change::Tool {
                    update: ToolUpdate {
                        key: key.clone(),
                        identity: stream.identity.clone(),
                        operation: ToolOperation::Interrupt,
                        text: self.policy.scrub_tool(""),
                        fingerprint: None,
                    },
                });
            }
        }
    }
}
