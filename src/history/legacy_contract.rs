//! Explicit read capability manifest, independent of migration version gates.
use super::legacy_reader::Collection;

pub(super) struct Contract {
    pub kind: Collection,
    pub table: &'static str,
    pub columns: &'static str,
}
pub(super) const CONTRACTS: &[Contract] = &[
    Contract {
        kind: Collection::Projects,
        table: "threads",
        columns: "project_key cwd",
    },
    Contract {
        kind: Collection::Threads,
        table: "threads",
        columns: "thread_key codex_thread_id store_source_id name cwd project_key source model archived runtime_status runtime_status_stale capture_completeness completeness_reasons_json created_at_ms updated_at_ms recency_at_ms last_message_preview last_event_seq",
    },
    Contract {
        kind: Collection::Context,
        table: "threads",
        columns: "thread_key parent_thread_id parent_thread_key forked_from_id forked_from_thread_key agent_nickname agent_role agent_path originator cli_version thread_source history_mode history_base_json model_provider reasoning_effort approval_policy approvals_reviewer_json sandbox_json active_permission_profile_json base_instructions_json dynamic_tools_json selected_capability_roots_json memory_mode subagent_history_start_ordinal multi_agent_version context_window_json provenance_json",
    },
    Contract {
        kind: Collection::Relations,
        table: "threads",
        columns: "thread_key parent_thread_key forked_from_thread_key codex_thread_id name",
    },
    Contract {
        kind: Collection::Turns,
        table: "turns",
        columns: "thread_key turn_id status capture_completeness completeness_reasons_json coverage_json started_at_ms completed_at_ms execution_context_json projection_json provenance_json last_event_seq",
    },
    Contract {
        kind: Collection::Items,
        table: "items",
        columns: "thread_key turn_scope item_id turn_id item_type status started_at_ms completed_at_ms summary_text projection_json provenance_json last_event_seq",
    },
    // config_json and stable_identity intentionally excluded: source identity
    // and status suffice; credentials/registered paths are not a history DTO.
    Contract {
        kind: Collection::Sources,
        table: "sources",
        columns: "source_id kind status last_seen_at_ms created_at_ms updated_at_ms",
    },
    Contract {
        kind: Collection::Epochs,
        table: "source_epochs",
        columns: "source_id epoch_id opened_at_ms closed_at_ms close_reason capability_json",
    },
    Contract {
        kind: Collection::Gaps,
        table: "ingest_errors",
        columns: "error_id source_id epoch_id offset_or_seq category first_seen_at_ms last_seen_at_ms occurrence_count",
    },
    Contract {
        kind: Collection::Raw,
        table: "raw_events",
        columns: "event_seq event_id source_id epoch_id source_seq thread_key codex_thread_id turn_id item_id observed_at_ms event_at_ms method phase durability raw_json redaction_json decode_status blob_id stored_raw_hash",
    },
    Contract {
        kind: Collection::Blobs,
        table: "blobs",
        columns: "blob_id stored_hash media_type size_bytes relative_path redaction_json created_event_seq",
    },
];

pub(super) fn contract(kind: Collection) -> &'static Contract {
    CONTRACTS
        .iter()
        .find(|contract| contract.kind == kind)
        .expect("exhaustive collection manifest")
}

pub(super) fn field_name(column: &str) -> String {
    let column = match column {
        "projection_json" | "raw_json" => "raw",
        "runtime_status" => "status",
        "runtime_status_stale" => "stale",
        _ => column.strip_suffix("_json").unwrap_or(column),
    };
    let mut segments = column.split('_');
    let mut name = segments.next().unwrap_or("").to_owned();
    for segment in segments {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            name.extend(first.to_uppercase());
        }
        name.extend(chars);
    }
    name
}

pub(super) fn numeric(column: &str) -> bool {
    column.ends_with("_at_ms")
        || column.ends_with("_seq")
        || matches!(
            column,
            "archived"
                | "runtime_status_stale"
                | "subagent_history_start_ordinal"
                | "occurrence_count"
                | "size_bytes"
                | "thread_count"
        )
}
