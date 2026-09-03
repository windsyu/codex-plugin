#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorkerRegistration {
    pub worker_id: String,
    pub create_command_id: String,
    pub principal_id: String,
    pub source_id: String,
    pub source_epoch: String,
    pub mode: String,
    pub canonical_cwd: String,
    pub rows: u16,
    pub cols: u16,
    pub runtime_dir_name: String,
    pub primary_lease_id: String,
    pub codex_thread_id: Option<String>,
    pub reservation_id: Option<String>,
}

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEpochStale {
    pub source_id: String,
    pub source_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWorkerRecord {
    pub worker_id: String,
    pub create_command_id: String,
    pub principal_id: String,
    pub source_id: String,
    pub source_epoch: String,
    pub mode: String,
    pub state: String,
    pub version: i64,
    pub input_lease_version: i64,
    pub primary_thread_id: Option<String>,
    pub canonical_cwd: String,
    pub rows: u16,
    pub cols: u16,
    pub pid: Option<u32>,
    pub runtime_dir_name: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLeaseRecord {
    pub lease_id: String,
    pub source_id: String,
    pub source_epoch: String,
    pub codex_thread_id: Option<String>,
    pub reservation_id: Option<String>,
    pub worker_id: String,
    pub role: String,
    pub state: String,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachmentRecord {
    pub attachment_id: String,
    pub worker_id: String,
    pub principal_id: String,
    pub state: String,
    pub version: i64,
    pub last_ack: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputLeaseRecord {
    pub lease_id: String,
    pub worker_id: String,
    pub owner_type: String,
    pub owner_id: String,
    pub state: String,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnOwnerRecord {
    pub source_id: String,
    pub source_epoch: String,
    pub worker_id: String,
    pub codex_thread_id: String,
    pub codex_turn_id: String,
    pub owner_type: String,
    pub owner_id: String,
    pub principal_id: String,
    pub input_lease_id: Option<String>,
    pub state: String,
    pub version: i64,
    pub start_command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteTurnOwner {
    pub source_id: String,
    pub source_epoch: String,
    pub codex_thread_id: String,
    pub codex_turn_id: String,
    pub to_state: String,
    pub reason_code: String,
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterSessionWorker {
    Created {
        worker: Box<SessionWorkerRecord>,
        lease: Box<ThreadLeaseRecord>,
    },
    ThreadOwned {
        worker_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireThreadLease {
    Acquired(ThreadLeaseRecord),
    Existing(ThreadLeaseRecord),
    ThreadOwned { worker_id: String },
    VersionConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWorkerTransition {
    pub worker_id: String,
    pub expected_version: i64,
    pub to_state: String,
    pub pid: Option<u32>,
    pub primary_thread_id: Option<String>,
    pub error_code: Option<String>,
    pub reason_code: Option<String>,
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistInputLease {
    Acquired { lease_id: String, version: i64 },
    Released { version: i64 },
    Conflict { version: i64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecoveryReport {
    pub orphaned_workers: usize,
    pub orphaned_thread_leases: usize,
    pub orphaned_attachments: usize,
    pub orphaned_input_leases: usize,
}
