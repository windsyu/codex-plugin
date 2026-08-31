use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::domain::gateway::GatewayCommandRecord;

pub(crate) const ACTOR_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug)]
pub(crate) enum ActorRequest {
    Snapshot {
        reply: oneshot::Sender<SourceActorSnapshot>,
    },
    Dispatch {
        command: ActorCommand,
        reply: oneshot::Sender<GatewayCommandRecord>,
    },
    Catalog {
        thread_id: Option<String>,
        thread_key: Option<String>,
        reply: oneshot::Sender<SourceControlCatalog>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActorCommand {
    pub command_id: String,
    pub operation: ControllerOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerOperation {
    ThreadStart {
        cwd: String,
        model: Option<String>,
        personality: Option<String>,
        permissions: Option<String>,
    },
    ThreadResume {
        thread_id: String,
        thread_key: String,
    },
    ThreadFork {
        thread_id: String,
        thread_key: String,
        last_turn_id: Option<String>,
    },
    TurnStart {
        thread_id: String,
        thread_key: String,
        client_user_message_id: String,
        text: String,
        image_paths: Vec<String>,
    },
    TurnSteer {
        thread_id: String,
        thread_key: String,
        expected_turn_id: String,
        client_user_message_id: String,
        text: String,
        image_paths: Vec<String>,
    },
    TurnInterrupt {
        thread_id: String,
        thread_key: String,
        expected_turn_id: String,
    },
    ThreadSettingsUpdate {
        thread_id: String,
        thread_key: String,
        setting: ThreadSetting,
    },
    Plan {
        thread_id: String,
        thread_key: String,
        expected_turn_id: Option<String>,
        client_user_message_id: Option<String>,
        prompt: Option<String>,
    },
    ThreadNameSet {
        thread_id: String,
        thread_key: String,
        name: String,
    },
    ThreadArchive {
        thread_id: String,
        thread_key: String,
    },
    ThreadCompact {
        thread_id: String,
        thread_key: String,
    },
    ReviewStart {
        thread_id: String,
        thread_key: String,
        target: ReviewTarget,
    },
    GoalGet {
        thread_id: String,
        thread_key: String,
    },
    GoalSet {
        thread_id: String,
        thread_key: String,
        objective: Option<String>,
        status: Option<String>,
    },
    GoalClear {
        thread_id: String,
        thread_key: String,
    },
    PendingRequestAction {
        thread_id: String,
        thread_key: String,
        request_id: String,
        expected_request_version: i64,
        action: PendingRequestAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThreadSetting {
    Model(String),
    ReasoningEffort(String),
    Personality(String),
    Permissions(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewTarget {
    UncommittedChanges,
    BaseBranch(String),
    Commit { sha: String, title: Option<String> },
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingRequestAction {
    Approval {
        decision: String,
    },
    Permissions {
        grant: bool,
        scope: String,
        strict_auto_review: Option<bool>,
    },
    UserInput {
        answers: BTreeMap<String, Vec<String>>,
    },
    McpElicitation {
        action: String,
        content: Option<Value>,
    },
}

impl ReviewTarget {
    fn protocol_value(&self) -> Value {
        match self {
            Self::UncommittedChanges => json!({"type":"uncommittedChanges"}),
            Self::BaseBranch(branch) => json!({"type":"baseBranch","branch":branch}),
            Self::Commit { sha, title } => json!({"type":"commit","sha":sha,"title":title}),
            Self::Custom(instructions) => {
                json!({"type":"custom","instructions":instructions})
            }
        }
    }
}

impl ControllerOperation {
    pub(crate) fn method_and_params(&self) -> Option<(&'static str, Value)> {
        Some(match self {
            Self::ThreadStart {
                cwd,
                model,
                personality,
                permissions,
            } => (
                "thread/start",
                json!({
                    "cwd":cwd,
                    "model":model,
                    "personality":personality,
                    "permissions":permissions,
                    "experimentalRawEvents":false,
                }),
            ),
            Self::ThreadResume {
                thread_id,
                thread_key: _,
            } => ("thread/resume", json!({"threadId":thread_id})),
            Self::ThreadFork {
                thread_id,
                thread_key: _,
                last_turn_id,
            } => (
                "thread/fork",
                json!({"threadId":thread_id,"lastTurnId":last_turn_id}),
            ),
            Self::TurnStart {
                thread_id,
                thread_key: _,
                client_user_message_id,
                text,
                image_paths,
            } => (
                "turn/start",
                json!({
                    "threadId":thread_id,
                    "input":turn_input(text,image_paths),
                    "clientUserMessageId":client_user_message_id,
                }),
            ),
            Self::TurnSteer {
                thread_id,
                thread_key: _,
                expected_turn_id,
                client_user_message_id,
                text,
                image_paths,
            } => (
                "turn/steer",
                json!({
                    "threadId":thread_id,
                    "expectedTurnId":expected_turn_id,
                    "input":turn_input(text,image_paths),
                    "clientUserMessageId":client_user_message_id,
                }),
            ),
            Self::TurnInterrupt {
                thread_id,
                thread_key: _,
                expected_turn_id,
            } => (
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":expected_turn_id}),
            ),
            Self::ThreadSettingsUpdate {
                thread_id,
                thread_key: _,
                setting,
            } => {
                let mut params = serde_json::Map::from_iter([(
                    "threadId".into(),
                    Value::String(thread_id.clone()),
                )]);
                let (key, value) = match setting {
                    ThreadSetting::Model(value) => ("model", value),
                    ThreadSetting::ReasoningEffort(value) => ("effort", value),
                    ThreadSetting::Personality(value) => ("personality", value),
                    ThreadSetting::Permissions(value) => ("permissions", value),
                };
                params.insert(key.into(), Value::String(value.clone()));
                ("thread/settings/update", Value::Object(params))
            }
            Self::Plan { .. } => return None,
            Self::ThreadNameSet {
                thread_id, name, ..
            } => ("thread/name/set", json!({"threadId":thread_id,"name":name})),
            Self::ThreadArchive { thread_id, .. } => {
                ("thread/archive", json!({"threadId":thread_id}))
            }
            Self::ThreadCompact { thread_id, .. } => {
                ("thread/compact/start", json!({"threadId":thread_id}))
            }
            Self::ReviewStart {
                thread_id, target, ..
            } => (
                "review/start",
                json!({"threadId":thread_id,"target":target.protocol_value()}),
            ),
            Self::GoalGet { thread_id, .. } => ("thread/goal/get", json!({"threadId":thread_id})),
            Self::GoalSet {
                thread_id,
                objective,
                status,
                ..
            } => (
                "thread/goal/set",
                json!({"threadId":thread_id,"objective":objective,"status":status}),
            ),
            Self::GoalClear { thread_id, .. } => {
                ("thread/goal/clear", json!({"threadId":thread_id}))
            }
            Self::PendingRequestAction { .. } => return None,
        })
    }

    pub(crate) fn thread_target(&self) -> Option<(&str, &str)> {
        match self {
            Self::ThreadStart { .. } => None,
            Self::ThreadResume {
                thread_id,
                thread_key,
            }
            | Self::ThreadFork {
                thread_id,
                thread_key,
                ..
            }
            | Self::TurnStart {
                thread_id,
                thread_key,
                ..
            }
            | Self::TurnSteer {
                thread_id,
                thread_key,
                ..
            }
            | Self::TurnInterrupt {
                thread_id,
                thread_key,
                ..
            }
            | Self::ThreadSettingsUpdate {
                thread_id,
                thread_key,
                ..
            }
            | Self::Plan {
                thread_id,
                thread_key,
                ..
            }
            | Self::ThreadNameSet {
                thread_id,
                thread_key,
                ..
            }
            | Self::ThreadArchive {
                thread_id,
                thread_key,
            }
            | Self::ThreadCompact {
                thread_id,
                thread_key,
            }
            | Self::ReviewStart {
                thread_id,
                thread_key,
                ..
            }
            | Self::GoalGet {
                thread_id,
                thread_key,
            }
            | Self::GoalSet {
                thread_id,
                thread_key,
                ..
            }
            | Self::GoalClear {
                thread_id,
                thread_key,
            }
            | Self::PendingRequestAction {
                thread_id,
                thread_key,
                ..
            } => Some((thread_id, thread_key)),
        }
    }
}

fn turn_input(text: &str, image_paths: &[String]) -> Vec<Value> {
    let mut input = Vec::with_capacity(1 + image_paths.len());
    if !text.is_empty() {
        input.push(json!({"type":"text","text":text}));
    }
    input.extend(
        image_paths
            .iter()
            .map(|path| json!({"type":"localImage","path":path,"detail":null})),
    );
    input
}

pub(crate) fn actor_channel() -> (mpsc::Sender<ActorRequest>, mpsc::Receiver<ActorRequest>) {
    mpsc::channel(ACTOR_CHANNEL_CAPACITY)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilityEntry {
    pub available: bool,
    pub experimental: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilityCatalog {
    pub entries: BTreeMap<String, CapabilityEntry>,
}

impl CapabilityCatalog {
    pub(crate) fn record_response(&mut self, method: &str, experimental: bool, envelope: &Value) {
        let (available, data, error_code) = if let Some(result) = envelope.get("result") {
            (true, Some(result.clone()), None)
        } else {
            (
                false,
                None,
                envelope
                    .pointer("/error/code")
                    .map(|code| {
                        code.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| code.to_string())
                    })
                    .or_else(|| Some("UPSTREAM_REJECTED".into())),
            )
        };
        self.entries.insert(
            method.into(),
            CapabilityEntry {
                available,
                experimental,
                data,
                error_code,
            },
        );
    }

    pub(crate) fn record_availability(
        &mut self,
        method: &str,
        experimental: bool,
        envelope: &Value,
    ) {
        let available = envelope.get("result").is_some();
        let error_code = (!available).then(|| {
            envelope
                .pointer("/error/code")
                .map(|code| {
                    code.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| code.to_string())
                })
                .unwrap_or_else(|| "UPSTREAM_REJECTED".into())
        });
        self.entries.insert(
            method.into(),
            CapabilityEntry {
                available,
                experimental,
                data: None,
                error_code,
            },
        );
    }

    pub(crate) fn summary(&self) -> Value {
        Value::Object(
            self.entries
                .iter()
                .map(|(method, entry)| {
                    (
                        method.clone(),
                        json!({
                            "available":entry.available,
                            "experimental":entry.experimental,
                            "itemCount":entry.data.as_ref()
                                .and_then(|data| data.get("data"))
                                .and_then(Value::as_array)
                                .map(Vec::len)
                        }),
                    )
                })
                .collect(),
        )
    }

    pub(crate) fn selectable_id(&self, method: &str, id: &str) -> bool {
        self.catalog_items(method).is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(id)
                    && entry.get("allowed").and_then(Value::as_bool) != Some(false)
                    && entry.get("hidden").and_then(Value::as_bool) != Some(true)
            })
        })
    }

    pub(crate) fn model_supports_effort(&self, model_id: &str, effort: &str) -> bool {
        self.catalog_items("model/list").is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(model_id)
                    && entry
                        .get("supportedReasoningEfforts")
                        .and_then(Value::as_array)
                        .is_some_and(|efforts| {
                            efforts.iter().any(|entry| {
                                entry.get("reasoningEffort").and_then(Value::as_str) == Some(effort)
                            })
                        })
            })
        })
    }

    pub(crate) fn model_supports_personality(&self, model_id: &str) -> bool {
        self.catalog_items("model/list").is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(model_id)
                    && entry
                        .get("supportsPersonality")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            })
        })
    }

    pub(crate) fn collaboration_mode(
        &self,
        mode: &str,
        current_model: Option<&str>,
    ) -> Option<Value> {
        let preset = self
            .catalog_items("collaborationMode/list")?
            .iter()
            .find(|entry| entry.get("mode").and_then(Value::as_str) == Some(mode))?;
        let model = preset
            .get("model")
            .and_then(Value::as_str)
            .or(current_model)?;
        if !self.selectable_id("model/list", model) {
            return None;
        }
        let effort = preset.get("reasoning_effort").and_then(Value::as_str);
        if effort.is_some_and(|effort| !self.model_supports_effort(model, effort)) {
            return None;
        }
        Some(json!({
            "mode":mode,
            "settings":{
                "model":model,
                "reasoning_effort":effort,
                "developer_instructions":null,
            }
        }))
    }

    fn catalog_items(&self, method: &str) -> Option<&Vec<Value>> {
        self.entries
            .get(method)
            .filter(|entry| entry.available)
            .and_then(|entry| entry.data.as_ref())
            .and_then(|data| data.get("data"))
            .and_then(Value::as_array)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceActorSnapshot {
    pub source_id: String,
    pub source_epoch: String,
    pub state: String,
    pub experimental_api: bool,
    pub catalog: CapabilityCatalog,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceControlCatalog {
    pub source_id: String,
    pub source_epoch: String,
    pub thread_loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<Value>,
    pub capabilities: CapabilityCatalog,
    pub slash_commands: Vec<SlashCommandEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SlashCommandEntry {
    pub name: String,
    pub capability: String,
    pub interaction_required_without_argument: bool,
}

#[derive(Clone)]
struct RegistryEntry {
    snapshot: SourceActorSnapshot,
    sender: Option<mpsc::Sender<ActorRequest>>,
}

#[derive(Clone, Default)]
pub(crate) struct ControllerRegistry {
    entries: Arc<RwLock<BTreeMap<String, RegistryEntry>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryError {
    SourceNotLive,
    SourceEpochStale,
}

impl ControllerRegistry {
    pub(crate) fn publish(
        &self,
        snapshot: SourceActorSnapshot,
        sender: mpsc::Sender<ActorRequest>,
    ) {
        self.entries
            .write()
            .expect("controller registry poisoned")
            .insert(
                snapshot.source_id.clone(),
                RegistryEntry {
                    snapshot,
                    sender: Some(sender),
                },
            );
    }

    pub(crate) fn mark_unavailable(&self, source_id: &str, source_epoch: &str, reason: &str) {
        let mut entries = self.entries.write().expect("controller registry poisoned");
        let Some(entry) = entries.get_mut(source_id) else {
            return;
        };
        if entry.snapshot.source_epoch != source_epoch {
            return;
        }
        entry.snapshot.state = "unavailable".into();
        entry.snapshot.unavailable_reason = Some(reason.into());
        entry.sender = None;
    }

    pub(crate) async fn snapshot(
        &self,
        source_id: &str,
        source_epoch: &str,
    ) -> Result<SourceActorSnapshot, RegistryError> {
        let sender = {
            let entries = self.entries.read().expect("controller registry poisoned");
            let entry = entries.get(source_id).ok_or(RegistryError::SourceNotLive)?;
            if entry.snapshot.source_epoch != source_epoch {
                return Err(RegistryError::SourceEpochStale);
            }
            entry.sender.clone().ok_or(RegistryError::SourceNotLive)?
        };
        let (reply, response) = oneshot::channel();
        sender
            .send(ActorRequest::Snapshot { reply })
            .await
            .map_err(|_| RegistryError::SourceNotLive)?;
        response.await.map_err(|_| RegistryError::SourceNotLive)
    }

    pub(crate) async fn dispatch(
        &self,
        source_id: &str,
        source_epoch: &str,
        command: ActorCommand,
    ) -> Result<GatewayCommandRecord, RegistryError> {
        let sender = {
            let entries = self.entries.read().expect("controller registry poisoned");
            let entry = entries.get(source_id).ok_or(RegistryError::SourceNotLive)?;
            if entry.snapshot.source_epoch != source_epoch {
                return Err(RegistryError::SourceEpochStale);
            }
            entry.sender.clone().ok_or(RegistryError::SourceNotLive)?
        };
        let (reply, response) = oneshot::channel();
        sender
            .send(ActorRequest::Dispatch { command, reply })
            .await
            .map_err(|_| RegistryError::SourceNotLive)?;
        response.await.map_err(|_| RegistryError::SourceNotLive)
    }

    pub(crate) async fn catalog(
        &self,
        source_id: &str,
        thread_id: Option<String>,
        thread_key: Option<String>,
    ) -> Result<SourceControlCatalog, RegistryError> {
        let sender = self
            .entries
            .read()
            .expect("controller registry poisoned")
            .get(source_id)
            .and_then(|entry| entry.sender.clone())
            .ok_or(RegistryError::SourceNotLive)?;
        let (reply, response) = oneshot::channel();
        sender
            .send(ActorRequest::Catalog {
                thread_id,
                thread_key,
                reply,
            })
            .await
            .map_err(|_| RegistryError::SourceNotLive)?;
        response.await.map_err(|_| RegistryError::SourceNotLive)
    }

    pub(crate) fn snapshots(&self) -> Vec<SourceActorSnapshot> {
        self.entries
            .read()
            .expect("controller registry poisoned")
            .values()
            .map(|entry| entry.snapshot.clone())
            .collect()
    }

    pub(crate) async fn resolved_snapshots(&self) -> Vec<SourceActorSnapshot> {
        let known = self.snapshots();
        let mut resolved = Vec::with_capacity(known.len());
        for snapshot in known {
            match self
                .snapshot(&snapshot.source_id, &snapshot.source_epoch)
                .await
            {
                Ok(live) => resolved.push(live),
                Err(RegistryError::SourceNotLive) => resolved.push(snapshot),
                Err(RegistryError::SourceEpochStale) => {}
            }
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    fn snapshot(epoch: &str) -> SourceActorSnapshot {
        SourceActorSnapshot {
            source_id: "source".into(),
            source_epoch: epoch.into(),
            state: "ready".into(),
            experimental_api: true,
            catalog: CapabilityCatalog::default(),
            unavailable_reason: None,
        }
    }

    #[test]
    fn catalog_fails_closed_for_rejected_method_without_leaking_error_body() {
        let mut catalog = CapabilityCatalog::default();
        catalog.record_response(
            "collaborationMode/list",
            true,
            &json!({"id":5,"error":{"code":-32601,"message":"private upstream detail"}}),
        );
        let entry = &catalog.entries["collaborationMode/list"];
        assert!(!entry.available);
        assert_eq!(entry.error_code.as_deref(), Some("-32601"));
        assert!(entry.data.is_none());
        assert!(
            !catalog
                .summary()
                .to_string()
                .contains("private upstream detail")
        );
    }

    #[test]
    fn plan_mode_requires_an_advertised_visible_model_and_supported_effort() {
        let mut catalog = CapabilityCatalog::default();
        catalog.record_response(
            "model/list",
            false,
            &json!({"result":{"data":[{
                "id":"model-a",
                "hidden":false,
                "supportedReasoningEfforts":[{"reasoningEffort":"high"}]
            }]}}),
        );
        catalog.record_response(
            "collaborationMode/list",
            true,
            &json!({"result":{"data":[{
                "name":"Plan",
                "mode":"plan",
                "model":"model-a",
                "reasoning_effort":"high"
            }]}}),
        );
        let mode = catalog
            .collaboration_mode("plan", None)
            .expect("advertised Plan mode");
        assert_eq!(mode["mode"], "plan");
        assert_eq!(mode["settings"]["model"], "model-a");
        assert_eq!(mode["settings"]["reasoning_effort"], "high");
        assert!(mode["settings"]["developer_instructions"].is_null());

        catalog.entries.get_mut("model/list").unwrap().data = Some(json!({"data":[{
            "id":"model-a",
            "hidden":true,
            "supportedReasoningEfforts":[{"reasoningEffort":"high"}]
        }]}));
        assert!(catalog.collaboration_mode("plan", None).is_none());
    }

    #[tokio::test]
    async fn registry_binds_snapshot_to_exact_epoch_and_marks_disconnect() -> Result<()> {
        let registry = ControllerRegistry::default();
        let (sender, mut receiver) = actor_channel();
        registry.publish(snapshot("epoch-1"), sender);
        let actor_snapshot = snapshot("epoch-1");
        let actor = tokio::spawn(async move {
            if let Some(request) = receiver.recv().await {
                match request {
                    ActorRequest::Snapshot { reply } => {
                        let _ = reply.send(actor_snapshot);
                    }
                    ActorRequest::Dispatch { .. } => unreachable!(),
                    ActorRequest::Catalog { .. } => unreachable!(),
                }
            }
        });
        assert_eq!(
            registry
                .snapshot("source", "epoch-1")
                .await
                .map(|snapshot| snapshot.source_epoch),
            Ok("epoch-1".into())
        );
        actor.await?;

        let (sender, _receiver) = actor_channel();
        registry.publish(snapshot("epoch-2"), sender);
        assert_eq!(
            registry.snapshot("source", "epoch-1").await.unwrap_err(),
            RegistryError::SourceEpochStale
        );
        assert_eq!(
            registry
                .dispatch(
                    "source",
                    "epoch-1",
                    ActorCommand {
                        command_id: "command".into(),
                        operation: ControllerOperation::ThreadStart {
                            cwd: "/synthetic/workspace".into(),
                            model: None,
                            personality: None,
                            permissions: None,
                        },
                    },
                )
                .await
                .unwrap_err(),
            RegistryError::SourceEpochStale
        );
        registry.mark_unavailable("source", "epoch-1", "stale disconnect");
        assert_eq!(registry.snapshots()[0].state, "ready");
        registry.mark_unavailable("source", "epoch-2", "transport closed");
        assert_eq!(registry.snapshots()[0].state, "unavailable");
        assert_eq!(
            registry.snapshot("source", "epoch-2").await.unwrap_err(),
            RegistryError::SourceNotLive
        );
        Ok(())
    }

    #[test]
    fn controller_operations_emit_only_fixed_protocol_methods() {
        let operation = ControllerOperation::TurnSteer {
            thread_id: "thread".into(),
            thread_key: "key".into(),
            expected_turn_id: "turn".into(),
            client_user_message_id: "message".into(),
            text: "hello".into(),
            image_paths: vec!["/private/staging/image.bin".into()],
        };
        let (method, params) = operation
            .method_and_params()
            .expect("turn steer has one fixed protocol request");
        assert_eq!(method, "turn/steer");
        assert_eq!(params["expectedTurnId"], "turn");
        assert_eq!(params["input"][0]["type"], "text");
        assert_eq!(
            params["input"][1],
            json!({"type":"localImage","path":"/private/staging/image.bin","detail":null})
        );
        assert!(params.get("method").is_none());
    }
}
