use super::tools::{ToolOutputStreams, ToolResultSource, ToolResultView};
use super::*;
use crate::workbench::decode::request::RequestPurpose;
use crate::workbench::decode::tool::{ArgumentState, ToolKind};
use crate::workbench::decode::tool_context::ToolCategory;
use crate::workbench::rollout::patches::NativePatchStatus;
use std::collections::HashMap;

impl LiveHub {
    pub(crate) fn apply_native_file_change(&self, record: NativeFileChange) {
        let mut state = self.state.lock().unwrap();
        if !state.requests.iter().any(|request| {
            request.info.purpose == RequestPurpose::Conversation
                && request.info.codex_thread_id == Some(record.key.codex_thread_id)
                && request.info.codex_turn_id.as_ref() == Some(&record.key.codex_turn_id)
        }) {
            return;
        }
        if state
            .native_file_changes
            .iter()
            .any(|existing| existing.same_evidence(&record))
        {
            return;
        }
        state.native_file_change_bytes += record.bytes();
        state.native_file_changes.push_back(record.clone());
        while state.native_file_changes.len() > 128
            || state.native_file_change_bytes > 4 * 1024 * 1024
        {
            state.native_file_change_bytes -=
                state.native_file_changes.pop_front().unwrap().bytes();
            state.partial = true;
            invalidate(&mut state);
        }
        let at = Instant::now();
        self.publish(
            &mut state,
            None,
            0,
            at,
            ViewChange::NativeFileChange { change: record },
        );
        self.reconcile_tools(&mut state, at);
    }
}
pub(super) struct Evidence {
    record: NativeFileChange,
    conflict: bool,
}
pub(super) fn matches(state: &State) -> HashMap<usize, Evidence> {
    let calls = super::native_tools::call_indices(state);
    let mut evidence: HashMap<usize, Evidence> = HashMap::new();
    for record in &state.native_file_changes {
        let Some(indices) = calls.get(&record.key) else {
            continue;
        };
        for index in indices {
            if let Some(existing) = evidence.get_mut(index) {
                existing.conflict |= indices.len() != 1 || !existing.record.same_evidence(record);
            } else {
                evidence.insert(
                    *index,
                    Evidence {
                        record: record.clone(),
                        conflict: indices.len() != 1,
                    },
                );
            }
        }
    }
    evidence
}
pub(super) fn apply(call: &mut ToolCallView, evidence: &Evidence) {
    if call.category != ToolCategory::Patch
        || call.identity.tool_kind != ToolKind::Custom
        || call.identity.name.as_deref() != Some("apply_patch")
        || call.arguments_state != ArgumentState::Generated
        || call.identity_conflict
    {
        return;
    }
    if evidence.conflict {
        call.result_conflict = true;
        return;
    }
    let record = &evidence.record;
    call.execution = match record.status {
        NativePatchStatus::Completed => ExecutionState::Succeeded,
        NativePatchStatus::Failed => ExecutionState::Failed,
        NativePatchStatus::Declined => ExecutionState::Declined,
    };
    call.result = Some(ToolResultView {
        source: ToolResultSource::NativeRollout {
            source_ref: record.source.source_ref,
            byte_offset: record.source.byte_offset,
            native_item_id: record.key.native_item_id.clone(),
            process_id: None,
        },
        output: String::new(),
        streams: Some(ToolOutputStreams {
            stdout: record.stdout.clone(),
            stderr: record.stderr.clone(),
        }),
        exit_code: None,
        duration_ms: None,
        truncated: record.truncated,
        omitted: record.omitted,
    });
}
