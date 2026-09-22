use super::tools::{ToolResultSource, ToolResultView};
use super::*;
use crate::workbench::decode::request::RequestPurpose;
use crate::workbench::rollout::UserKey;
use crate::workbench::rollout::tools::NativeCommandStatus;
use std::collections::HashMap;

impl LiveHub {
    pub(crate) fn apply_native_command(&self, command: NativeCommand) {
        let mut state = self.state.lock().unwrap();
        if !state.requests.iter().any(|request| {
            request.info.purpose == RequestPurpose::Conversation
                && request.info.codex_thread_id == Some(command.key.codex_thread_id)
                && request.info.codex_turn_id.as_ref() == Some(&command.key.codex_turn_id)
        }) {
            return;
        }
        if state
            .native_commands
            .iter()
            .any(|existing| existing.same_evidence(&command))
        {
            return;
        }
        state.native_command_bytes += command.bytes();
        state.native_commands.push_back(command.clone());
        while state.native_commands.len() > 128 || state.native_command_bytes > 4 * 1024 * 1024 {
            state.native_command_bytes -= state.native_commands.pop_front().unwrap().bytes();
            state.partial = true;
            invalidate(&mut state);
        }
        let at = Instant::now();
        self.publish(
            &mut state,
            None,
            0,
            at,
            ViewChange::NativeCommand { command },
        );
        self.reconcile_tools(&mut state, at);
    }
}

pub(super) struct Evidence {
    record: NativeCommand,
    conflict: bool,
}

pub(super) fn call_indices(state: &State) -> HashMap<UserKey, Vec<usize>> {
    let domains: HashMap<_, _> = state
        .requests
        .iter()
        .filter_map(|request| {
            if request.info.client_request_index.is_some()
                || request.info.purpose != RequestPurpose::Conversation
            {
                return None;
            }
            Some((
                request.request_id,
                (
                    request.info.codex_thread_id?,
                    request.info.codex_turn_id.as_ref()?,
                ),
            ))
        })
        .collect();
    let mut calls: HashMap<UserKey, Vec<usize>> = HashMap::new();
    for (index, call) in state.tools.iter().enumerate() {
        if let (Some((thread, turn)), Some(id)) =
            (domains.get(&call.key.request_id), &call.identity.call_id)
        {
            calls
                .entry(UserKey {
                    codex_thread_id: *thread,
                    codex_turn_id: (*turn).clone(),
                    native_item_id: id.clone(),
                })
                .or_default()
                .push(index);
        }
    }
    calls
}

pub(super) fn matches(state: &State) -> HashMap<usize, Evidence> {
    let calls = call_indices(state);
    let mut evidence: HashMap<usize, Evidence> = HashMap::new();
    for record in &state.native_commands {
        let Some(indices) = calls.get(&record.key) else {
            continue;
        };
        for index in indices {
            let conflict = indices.len() != 1;
            if let Some(existing) = evidence.get_mut(index) {
                existing.conflict |= conflict;
                match (existing.record.status, record.status) {
                    (NativeCommandStatus::InProgress, NativeCommandStatus::InProgress) => {
                        existing.conflict |= !existing.record.same_evidence(record);
                    }
                    (NativeCommandStatus::InProgress, _) => {
                        existing.conflict |= existing.record.process_id != record.process_id;
                        existing.record = record.clone();
                    }
                    (_, NativeCommandStatus::InProgress) => {
                        existing.conflict |= existing.record.process_id != record.process_id;
                    }
                    _ => existing.conflict |= !existing.record.same_evidence(record),
                }
            } else {
                evidence.insert(
                    *index,
                    Evidence {
                        record: record.clone(),
                        conflict,
                    },
                );
            }
        }
    }
    evidence
}

pub(super) fn apply(call: &mut ToolCallView, evidence: &Evidence) {
    use crate::workbench::decode::tool::{ArgumentState, ToolKind};
    use crate::workbench::decode::tool_context::ToolCategory;
    let record = &evidence.record;
    // No prefix or Code Mode source scanning: a nested execution is retained
    // separately unless a model call with this exact native ID can be proven.
    if call.category != ToolCategory::Command
        || call.identity.tool_kind != ToolKind::Function
        || call.identity.name.as_deref() != Some("exec_command")
        || call.arguments_state != ArgumentState::Generated
        || call.identity_conflict
        || !matches!(
            record.command_source.as_str(),
            "agent" | "unified_exec_startup"
        )
    {
        return;
    }
    if evidence.conflict
        || call
            .request_exit_code
            .zip(record.exit_code)
            .is_some_and(|(left, right)| left != right)
        || call
            .request_process_id
            .as_ref()
            .is_some_and(|id| record.process_id.as_ref() != Some(id))
    {
        call.result_conflict = true;
        return;
    }
    if record.status == NativeCommandStatus::InProgress {
        if call.result.is_none() {
            call.execution = ExecutionState::Running;
        }
        return;
    }
    if let Some(command) = &mut call.command
        && !record.cwd.is_empty()
    {
        command.cwd = Some(record.cwd.clone());
    }
    call.execution = match record.status {
        NativeCommandStatus::Completed => ExecutionState::Succeeded,
        NativeCommandStatus::Failed => ExecutionState::Failed,
        NativeCommandStatus::Declined => ExecutionState::Declined,
        NativeCommandStatus::InProgress => unreachable!(),
    };
    call.result = Some(ToolResultView {
        streams: None,
        source: ToolResultSource::NativeRollout {
            source_ref: record.source.source_ref,
            byte_offset: record.source.byte_offset,
            native_item_id: record.key.native_item_id.clone(),
            process_id: record.process_id.clone(),
        },
        output: record.output.clone(),
        exit_code: record.exit_code,
        duration_ms: record.duration_ms,
        truncated: record.truncated,
        omitted: record.omitted,
    });
}
