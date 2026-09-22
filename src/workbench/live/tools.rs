use super::*;
use crate::workbench::decode::patch::{self, PatchIssue, ProposedPatch};
use crate::workbench::decode::tool::{
    ArgumentState, PREVIEW_BYTES, ToolIdentity, ToolOperation, ToolUpdate,
};
use crate::workbench::decode::tool_context::{ToolCategory, ToolContext};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandPreview {
    pub text: String,
    pub cwd: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Unobserved,
    Running,
    ResultObserved,
    Succeeded,
    Failed,
    Declined,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolResultSource {
    ModelRequest {
        request_id: Uuid,
        client_request_index: Option<u64>,
        input_index: u32,
    },
    NativeRollout {
        source_ref: Uuid,
        byte_offset: u64,
        native_item_id: String,
        process_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutputStreams {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultView {
    pub source: ToolResultSource,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<ToolOutputStreams>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<f64>,
    pub truncated: bool,
    pub omitted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub key: TextKey,
    pub kind: &'static str,
    #[serde(flatten)]
    pub identity: ToolIdentity,
    pub arguments: String,
    pub arguments_state: ArgumentState,
    pub category: ToolCategory,
    pub command: Option<CommandPreview>,
    pub proposed_patch: Option<ProposedPatch>,
    pub execution: ExecutionState,
    pub result: Option<ToolResultView>,
    pub revision: u64,
    pub order_index: u64,
    pub capture_seq: u64,
    pub truncated: bool,
    pub identity_conflict: bool,
    pub result_conflict: bool,
    #[serde(skip)]
    fingerprint: Option<blake3::Hash>,
    #[serde(skip)]
    request_result_fingerprint: Option<blake3::Hash>,
    #[serde(skip)]
    pub(super) request_exit_code: Option<i32>,
    #[serde(skip)]
    pub(super) request_process_id: Option<String>,
}
impl ToolCallView {
    fn bytes(&self) -> usize {
        self.arguments.len()
            + self.command.as_ref().map_or(0, |cmd| {
                cmd.text.len() + cmd.cwd.as_ref().map_or(0, String::len)
            })
            + self.result.as_ref().map_or(0, |result| {
                result.output.len()
                    + result.streams.as_ref().map_or(0, |streams| {
                        streams.stdout.as_ref().map_or(0, String::len)
                            + streams.stderr.as_ref().map_or(0, String::len)
                    })
            })
            + self.proposed_patch.as_ref().map_or(0, ProposedPatch::bytes)
    }

    fn refresh_proposal(&mut self, before: &Self) {
        if self.category == before.category
            && self.arguments_state == before.arguments_state
            && self.truncated == before.truncated
            && self.identity_conflict == before.identity_conflict
            && self.arguments == before.arguments
        {
            return;
        }
        self.proposed_patch = if self.category != ToolCategory::Patch {
            None
        } else if self.identity_conflict {
            Some(ProposedPatch::Unavailable {
                reason: PatchIssue::IdentityConflict,
            })
        } else if self.truncated {
            Some(ProposedPatch::Unavailable {
                reason: PatchIssue::Truncated,
            })
        } else {
            match self.arguments_state {
                ArgumentState::Receiving => None,
                ArgumentState::Incomplete => Some(ProposedPatch::Unavailable {
                    reason: PatchIssue::Incomplete,
                }),
                ArgumentState::Generated => Some(patch::preview(&self.arguments)),
            }
        };
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolContextView {
    pub request_id: Uuid,
    #[serde(flatten)]
    pub context: ToolContext,
    #[serde(skip)]
    bytes: usize,
}

impl LiveHub {
    pub(super) fn apply_tool(
        &self,
        state: &mut State,
        update: ToolUpdate,
        capture_seq: u64,
    ) -> Option<ViewChange> {
        let index = if let Some(index) = state.tools.iter().position(|call| call.key == update.key)
        {
            index
        } else {
            state.tools.push_back(ToolCallView {
                key: update.key.clone(),
                kind: "tool_call",
                identity: update.identity.clone(),
                arguments: String::new(),
                arguments_state: ArgumentState::Receiving,
                category: ToolCategory::Other,
                command: None,
                proposed_patch: None,
                execution: ExecutionState::Unobserved,
                result: None,
                revision: 0,
                order_index: state.sequence + 1,
                capture_seq,
                truncated: false,
                identity_conflict: false,
                result_conflict: false,
                fingerprint: None,
                request_result_fingerprint: None,
                request_exit_code: None,
                request_process_id: None,
            });
            state.tools.len() - 1
        };
        let call = &mut state.tools[index];
        let before = call.clone();
        call.identity_conflict |= call.identity.tool_kind != update.identity.tool_kind
            || update.identity.invalid_fields
            // Never keep proof for old arguments while replacing their preview
            // with a final payload whose identity cannot be verified.
            || (update.operation == ToolOperation::Complete
                && update.fingerprint.is_none()
                && call.fingerprint.is_some());
        for (old, new) in [
            (&mut call.identity.call_id, &update.identity.call_id),
            (&mut call.identity.name, &update.identity.name),
            (&mut call.identity.namespace, &update.identity.namespace),
        ] {
            if old.is_some() && new.is_some() && old != new {
                call.identity_conflict = true;
            }
            if old.is_none() {
                *old = new.clone();
            }
        }
        if let Some(fingerprint) = update.fingerprint {
            if call.fingerprint.is_some_and(|old| old != fingerprint) {
                call.identity_conflict = true;
            } else {
                call.fingerprint = Some(fingerprint);
            }
        }
        if !call.identity_conflict {
            let limit = self.limits.item_bytes.min(PREVIEW_BYTES);
            match update.operation {
                ToolOperation::Begin | ToolOperation::Append
                    if call.arguments_state != ArgumentState::Generated =>
                {
                    let append = prefix(
                        update.text.as_str(),
                        limit.saturating_sub(call.arguments.len()),
                    );
                    call.truncated |= append.len() < update.text.as_str().len();
                    call.arguments.push_str(append);
                }
                ToolOperation::Complete => {
                    let text = prefix(update.text.as_str(), limit);
                    call.arguments = text.to_owned();
                    call.truncated = text.len() < update.text.as_str().len();
                    call.arguments_state = ArgumentState::Generated;
                }
                ToolOperation::Interrupt if call.arguments_state != ArgumentState::Generated => {
                    call.arguments_state = ArgumentState::Incomplete
                }
                _ => {}
            }
        } else {
            state.partial = true;
        }
        call.refresh_proposal(&before);
        if *call == before && call.revision > 0 {
            return None;
        }
        call.revision += 1;
        state.tool_bytes = state.tool_bytes - before.bytes() + call.bytes();
        if update.operation == ToolOperation::Append
            && before.revision > 0
            && call.identity == before.identity
            && call.identity_conflict == before.identity_conflict
            && call.proposed_patch == before.proposed_patch
            && call.arguments.starts_with(&before.arguments)
        {
            return Some(ViewChange::Patch {
                item_key: view::tool_key(&call.key),
                base_revision: before.revision,
                revision: call.revision,
                patch: ItemPatch::Arguments {
                    append: call.arguments[before.arguments.len()..].to_owned(),
                },
                truncated: call.truncated,
            });
        }
        Some(ViewChange::Replace {
            item: ViewItem::Tool(Box::new(call.clone())),
        })
    }

    pub(super) fn apply_tool_context(
        &self,
        state: &mut State,
        request_id: Uuid,
        context: ToolContext,
    ) -> ViewChange {
        let bytes = serde_json::to_vec(&context)
            .expect("safe tool context")
            .len();
        let view = ToolContextView {
            request_id,
            context,
            bytes,
        };
        if let Some(index) = state.tool_contexts.iter().position(|existing| {
            existing.request_id == request_id
                && existing.context.client_request_index == view.context.client_request_index
        }) {
            state.tool_context_bytes -= state.tool_contexts.remove(index).unwrap().bytes;
        }
        state.partial |= view.context.partial;
        state.tool_context_bytes += bytes;
        state.tool_contexts.push_back(view.clone());
        while state.tool_contexts.len() > 64 || state.tool_context_bytes > 4 * 1024 * 1024 {
            state.tool_context_bytes -= state.tool_contexts.pop_front().unwrap().bytes;
            state.partial = true;
            invalidate(state);
        }
        ViewChange::ToolContext { context: view }
    }

    pub(super) fn trim_reading_items(&self, state: &mut State) {
        while state.items.len() + state.tools.len() > self.limits.items
            || state.text_bytes + state.tool_bytes > self.limits.view_bytes
        {
            let text_order = state
                .items
                .front()
                .map(|item| item.order_index)
                .unwrap_or(u64::MAX);
            let tool_order = state
                .tools
                .front()
                .map(|item| item.order_index)
                .unwrap_or(u64::MAX);
            if text_order <= tool_order {
                state.text_bytes -= state.items.pop_front().unwrap().text.len();
            } else {
                state.tool_bytes -= state.tools.pop_front().unwrap().bytes();
            }
            state.partial = true;
            invalidate(state);
        }
    }

    pub(super) fn reconcile_tools(&self, state: &mut State, received_at: Instant) {
        // Build the proven request domains and call/result index once. Repeated
        // history in subsequent requests must not cause a cross-product scan of
        // every tool, request and output on the observation thread.
        let domains: HashMap<_, _> = state
            .requests
            .iter()
            .filter_map(|request| {
                if request.info.client_request_index.is_some()
                    || request.info.purpose
                        == super::super::decode::request::RequestPurpose::Unknown
                {
                    return None;
                }
                Some((request.request_id, request.info.codex_thread_id?))
            })
            .collect();
        let mut candidates = HashMap::<(Uuid, blake3::Hash), usize>::new();
        for call in &state.tools {
            if !call.identity_conflict
                && let (Some(thread), Some(fingerprint)) =
                    (domains.get(&call.key.request_id), call.fingerprint)
            {
                *candidates.entry((*thread, fingerprint)).or_default() += 1;
            }
        }
        let mut outputs = HashMap::<_, Vec<_>>::new();
        for context in &state.tool_contexts {
            if context.context.client_request_index.is_some() {
                continue;
            }
            let Some(thread) = domains.get(&context.request_id) else {
                continue;
            };
            for output in &context.context.outputs {
                if let Some(fingerprint) = output.fingerprint {
                    outputs
                        .entry((*thread, fingerprint))
                        .or_default()
                        .push((context, output));
                }
            }
        }
        let mut changes = Vec::new();
        let native_matches = super::native_tools::matches(state);
        let native_patches = super::native_patches::matches(state);
        for index in 0..state.tools.len() {
            let before = &state.tools[index];
            let mut call = before.clone();
            if let Some(context) = state.tool_contexts.iter().find(|context| {
                context.request_id == call.key.request_id
                    && context.context.client_request_index.is_none()
            }) {
                call.category = ToolCategory::Other;
                let matching: Vec<_> = context
                    .context
                    .definitions
                    .iter()
                    .filter(|definition| {
                        Some(&definition.name) == call.identity.name.as_ref()
                            && definition.tool_kind == call.identity.tool_kind
                            && call.identity.namespace.as_ref().is_none_or(|namespace| {
                                Some(namespace) == definition.namespace.as_ref()
                            })
                    })
                    .collect();
                if let Some(first) = matching.first().filter(|first| {
                    matching.iter().all(|definition| {
                        definition.namespace == first.namespace
                            && definition.category == first.category
                    })
                }) {
                    call.category = first.category;
                }
            }
            if matches!(before.category, ToolCategory::Command | ToolCategory::Patch)
                && call.category != before.category
                && (before.result.is_some() || before.execution != ExecutionState::Unobserved)
            {
                call.result_conflict = true;
            }
            call.refresh_proposal(before);
            if call.category == ToolCategory::Command
                && call.arguments_state == ArgumentState::Generated
                && !call.truncated
                && !call.identity_conflict
                && let Ok(arguments) = serde_json::from_str::<serde_json::Value>(&call.arguments)
            {
                call.command = arguments["cmd"].as_str().map(|text| CommandPreview {
                    text: text.to_owned(),
                    cwd: arguments["workdir"].as_str().map(str::to_owned),
                });
            } else {
                call.command = None;
            }
            let thread = domains.get(&call.key.request_id).copied();
            if let (Some(thread), Some(fingerprint)) = (thread, call.fingerprint) {
                for (context, output) in outputs.get(&(thread, fingerprint)).into_iter().flatten() {
                    if output.companion.as_ref() != Some(&call.identity) {
                        continue;
                    }
                    if candidates.get(&(thread, fingerprint)).copied().unwrap_or(0) != 1
                        || call.identity_conflict
                    {
                        call.result_conflict = true;
                        continue;
                    }
                    let facts = (call.category == ToolCategory::Command)
                        .then_some(output.command_facts.as_ref())
                        .flatten();
                    let result = ToolResultView {
                        streams: None,
                        source: ToolResultSource::ModelRequest {
                            request_id: context.request_id,
                            client_request_index: context.context.client_request_index,
                            input_index: output.input_index,
                        },
                        output: if facts.is_some() {
                            output.command_output.as_ref().unwrap_or(&output.output)
                        } else {
                            &output.output
                        }
                        .as_str()
                        .to_owned(),
                        exit_code: facts.and_then(|facts| facts.exit_code),
                        duration_ms: facts.map(|facts| facts.duration_ms),
                        truncated: output.truncated,
                        omitted: output.omitted,
                    };
                    if let Some(existing) = call.request_result_fingerprint {
                        if existing != output.output_fingerprint {
                            call.result_conflict = true;
                        }
                    } else {
                        call.request_result_fingerprint = Some(output.output_fingerprint);
                        call.request_exit_code = result.exit_code;
                        call.request_process_id = facts.and_then(|facts| facts.process_id.clone());
                        if !matches!(
                            call.result.as_ref().map(|result| &result.source),
                            Some(ToolResultSource::NativeRollout { .. })
                        ) {
                            call.execution = match facts {
                                Some(facts) if facts.running => ExecutionState::Running,
                                Some(facts) if facts.exit_code == Some(0) => {
                                    ExecutionState::Succeeded
                                }
                                Some(facts) if facts.exit_code.is_some() => ExecutionState::Failed,
                                _ => ExecutionState::ResultObserved,
                            };
                            call.result = Some(result);
                        }
                    }
                }
            }
            if let Some(evidence) = native_matches.get(&index) {
                super::native_tools::apply(&mut call, evidence);
            }
            if let Some(evidence) = native_patches.get(&index) {
                super::native_patches::apply(&mut call, evidence);
            }
            if call.identity_conflict || call.result_conflict {
                call.execution = ExecutionState::Unobserved;
                state.partial = true;
            }
            if &call != before {
                call.revision += 1;
                state.tool_bytes = state.tool_bytes - before.bytes() + call.bytes();
                state.tools[index] = call.clone();
                changes.push(call);
            }
        }
        self.trim_reading_items(state);
        for tool in changes {
            if state.tools.iter().any(|current| current.key == tool.key) {
                self.publish(
                    state,
                    Some(tool.key.request_id),
                    tool.capture_seq,
                    received_at,
                    ViewChange::Replace {
                        item: ViewItem::Tool(Box::new(tool)),
                    },
                );
            }
        }
    }
}
