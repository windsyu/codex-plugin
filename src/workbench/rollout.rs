//! Read-only, bounded native user and tool evidence, off network/PTY paths.
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::live::LiveHub;
use super::redaction::RedactionPolicy;

mod files;
mod parse;
pub mod patches;
pub mod tools;

pub(crate) const USER_TEXT_BYTES: usize = 64 * 1024;
const PENDING_BYTES: usize = 1024 * 1024;
const FILES: usize = 8;
const READ_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserKey {
    pub codex_thread_id: Uuid,
    pub codex_turn_id: String,
    pub native_item_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSource {
    pub source_ref: Uuid,
    pub byte_offset: u64,
    pub ordinal: Option<u64>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub key: UserKey,
    pub role: &'static str,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
    pub omitted: bool,
    pub source: UserSource,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserIssue {
    InvalidLine,
    LineTooLarge,
    MissingIdentity,
    IdentityConflict,
    SourceChanged,
    ReadFailed,
    Capacity,
    UnsupportedToolEvidence,
    PartialLine,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDiagnostic {
    pub source_ref: Uuid,
    pub byte_offset: u64,
    pub code: UserIssue,
}

struct Tail {
    file: File,
    thread: Uuid,
    source: Uuid,
    lines: parse::Lines,
    verified: bool,
    failed: bool,
    identity: (u64, u64),
    // The last read bytes detect truncate/rewrite even when a file has regrown.
    guard: Vec<u8>,
}
impl Tail {
    fn new(file: File, thread: Uuid) -> io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self {
            file,
            thread,
            source: Uuid::new_v4(),
            lines: parse::Lines::new(1024 * 1024),
            verified: false,
            failed: false,
            identity: (meta.dev(), meta.ino()),
            guard: Vec::new(),
        })
    }
    fn read(
        &mut self,
        limit: usize,
        policy: &Arc<RedactionPolicy>,
        pending: &mut VecDeque<UserRecord>,
        commands: &mut VecDeque<tools::NativeCommand>,
        file_changes: &mut VecDeque<patches::NativeFileChange>,
        hub: &LiveHub,
    ) {
        use std::os::unix::fs::FileExt;
        if self.failed {
            return;
        }
        let offset = self.lines.offset;
        let unchanged = (|| -> io::Result<bool> {
            let metadata = self.file.metadata()?;
            if metadata.len() < offset || metadata.nlink() == 0 {
                return Ok(false);
            }
            if !self.guard.is_empty() {
                let mut check = vec![0; self.guard.len()];
                let start = offset - check.len() as u64;
                self.file.read_exact_at(&mut check, start)?;
                if check != self.guard {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        if !matches!(unchanged, Ok(true)) {
            self.failed = true;
            hub.user_issue(self.source, offset, UserIssue::SourceChanged);
            return;
        }
        let mut bytes = vec![0; limit];
        let read = match self.file.read(&mut bytes) {
            Ok(read) => read,
            Err(_) => {
                hub.user_issue(self.source, offset, UserIssue::ReadFailed);
                return;
            }
        };
        if read == 0 {
            return;
        }
        bytes.truncate(read);
        self.guard = bytes[bytes.len().saturating_sub(64)..].to_vec();
        let verified = &mut self.verified;
        let failed = &mut self.failed;
        let thread = self.thread;
        let source = self.source;
        self.lines.feed(&bytes, |offset, value| {
            if *failed {
                return;
            }
            let value = match value {
                Ok(value) => value,
                Err(code) => {
                    hub.user_issue(source, offset, code);
                    if !*verified {
                        *failed = true;
                    }
                    return;
                }
            };
            if !*verified {
                *verified = parse::session(&value, thread);
                if !*verified {
                    *failed = true;
                    hub.user_issue(source, offset, UserIssue::IdentityConflict);
                }
                return;
            }
            if value["type"] == "session_meta" {
                *failed = true;
                hub.user_issue(source, offset, UserIssue::SourceChanged);
                return;
            }
            match parse::user(&value, thread, source, offset, policy) {
                Ok(Some(record)) => {
                    pending.push_back(record);
                    while pending.len() > 128
                        || pending.iter().map(|item| item.text.len()).sum::<usize>() > PENDING_BYTES
                    {
                        let removed = pending.pop_front().unwrap();
                        hub.user_issue(
                            removed.source.source_ref,
                            removed.source.byte_offset,
                            UserIssue::Capacity,
                        );
                    }
                }
                Ok(None) => {}
                Err(code) => hub.user_issue(source, offset, code),
            }
            match tools::command(&value, thread, source, offset, policy) {
                Ok(Some(record)) => {
                    commands.push_back(record);
                    while commands.len() > 128
                        || commands
                            .iter()
                            .map(tools::NativeCommand::bytes)
                            .sum::<usize>()
                            > PENDING_BYTES
                    {
                        let removed = commands.pop_front().unwrap();
                        hub.user_issue(
                            removed.source.source_ref,
                            removed.source.byte_offset,
                            UserIssue::Capacity,
                        );
                    }
                }
                Ok(None) => {}
                Err(code) => hub.user_issue(source, offset, code),
            }
            match patches::file_change(&value, thread, source, offset, policy) {
                Ok(Some(record)) => {
                    file_changes.push_back(record);
                    while file_changes.len() > 128
                        || file_changes
                            .iter()
                            .map(patches::NativeFileChange::bytes)
                            .sum::<usize>()
                            > PENDING_BYTES
                    {
                        let removed = file_changes.pop_front().unwrap();
                        hub.user_issue(
                            removed.source.source_ref,
                            removed.source.byte_offset,
                            UserIssue::Capacity,
                        );
                    }
                }
                Ok(None) => {}
                Err(code) => hub.user_issue(source, offset, code),
            }
        });
    }
}

struct Reader {
    home: File,
    policy: Arc<RedactionPolicy>,
    tails: Vec<Tail>,
    pending: VecDeque<UserRecord>,
    pending_commands: VecDeque<tools::NativeCommand>,
    pending_file_changes: VecDeque<patches::NativeFileChange>,
    scan: Option<files::Scan>,
}
impl Reader {
    #[cfg(test)]
    fn new(home: &Path, policy: Arc<RedactionPolicy>) -> io::Result<Self> {
        Ok(Self {
            home: files::root(home)?,
            policy,
            tails: Vec::new(),
            pending: VecDeque::new(),
            pending_commands: VecDeque::new(),
            pending_file_changes: VecDeque::new(),
            scan: None,
        })
    }
    fn tick(&mut self, hub: &LiveHub) {
        let targets = hub.user_targets();
        if targets.is_empty() {
            return;
        }
        if self.scan.is_none() {
            match files::Scan::new(&self.home) {
                Ok(scan) => self.scan = Some(scan),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => hub.user_issue(Uuid::nil(), 0, UserIssue::ReadFailed),
            }
        }
        if let Some(scan) = &mut self.scan {
            let tails = &mut self.tails;
            let done = scan.step(2048, |parent, name| {
                let Some(name_str) = name.to_str() else {
                    return;
                };
                let Some(thread) = targets
                    .iter()
                    .map(|target| target.0)
                    .find(|thread| name_str.ends_with(&format!("-{thread}.jsonl")))
                else {
                    return;
                };
                let Ok(file) = files::child(parent, name, false) else {
                    hub.user_issue(Uuid::nil(), 0, UserIssue::ReadFailed);
                    return;
                };
                let Ok(meta) = file.metadata() else {
                    return;
                };
                if tails
                    .iter()
                    .any(|tail| tail.identity == (meta.dev(), meta.ino()))
                {
                    return;
                }
                if tails.iter().any(|tail| tail.thread == thread) {
                    // Never silently join a replacement/second file to a known source.
                    hub.user_issue(Uuid::nil(), 0, UserIssue::SourceChanged);
                    return;
                }
                if tails.len() == FILES {
                    hub.user_issue(Uuid::nil(), 0, UserIssue::Capacity);
                    return;
                }
                if let Ok(tail) = Tail::new(file, thread) {
                    tails.push(tail);
                }
            });
            match done {
                Ok(true) => self.scan = None,
                Ok(false) => {}
                Err(_) => {
                    self.scan = None;
                    hub.user_issue(Uuid::nil(), 0, UserIssue::ReadFailed);
                }
            }
        }
        let each = READ_BYTES / self.tails.len().max(1);
        for tail in &mut self.tails {
            tail.read(
                each,
                &self.policy,
                &mut self.pending,
                &mut self.pending_commands,
                &mut self.pending_file_changes,
                hub,
            );
        }
        self.pending.retain(|user| {
            if targets.iter().any(|(thread, turn)| {
                *thread == user.key.codex_thread_id && *turn == user.key.codex_turn_id
            }) {
                hub.apply_user(user.clone());
                false
            } else {
                true
            }
        });
        self.pending_commands.retain(|command| {
            if targets.iter().any(|(thread, turn)| {
                *thread == command.key.codex_thread_id && *turn == command.key.codex_turn_id
            }) {
                hub.apply_native_command(command.clone());
                false
            } else {
                true
            }
        });
        self.pending_file_changes.retain(|record| {
            if targets.iter().any(|(thread, turn)| {
                *thread == record.key.codex_thread_id && *turn == record.key.codex_turn_id
            }) {
                hub.apply_native_file_change(record.clone());
                false
            } else {
                true
            }
        });
    }
}

pub struct RolloutReader {
    stop: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl RolloutReader {
    pub fn start(home: &Path, hub: Arc<LiveHub>, policy: Arc<RedactionPolicy>) -> Result<Self> {
        Self::start_with_delay(home, hub, policy, Duration::ZERO)
    }
    /// Isolated fault-injection hook for late-source acceptance, never a browser input.
    pub fn start_with_delay(
        home: &Path,
        hub: Arc<LiveHub>,
        policy: Arc<RedactionPolicy>,
        delay: Duration,
    ) -> Result<Self> {
        // Canonicalization is explicit and once only; all later traversal uses fds.
        let canonical = home.canonicalize().context("resolve native history root")?;
        let home = files::root(&canonical).context("open native history root")?;
        let (stop, receiver) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        hub.enable_user_reader();
        let thread = std::thread::Builder::new()
            .name("native-rollout-reader".into())
            .spawn(move || {
                let mut reader = Reader {
                    home,
                    policy,
                    tails: Vec::new(),
                    pending: VecDeque::new(),
                    pending_commands: VecDeque::new(),
                    pending_file_changes: VecDeque::new(),
                    scan: None,
                };
                let mut first_target = None;
                loop {
                    if !hub.user_targets().is_empty() {
                        let since = first_target.get_or_insert_with(std::time::Instant::now);
                        if since.elapsed() >= delay {
                            reader.tick(&hub);
                        }
                    }
                    match receiver.recv_timeout(Duration::from_millis(200)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => {
                            // A final bounded scan can pick up completed native
                            // records written as the CLI exits. Never synthesize
                            // a submission or a tool outcome from a partial line.
                            reader.tick(&hub);
                            for tail in &reader.tails {
                                if let Some(offset) = tail.lines.partial_offset() {
                                    hub.user_issue(tail.source, offset, UserIssue::PartialLine);
                                }
                            }
                            break;
                        }
                    }
                }
                let _ = finished.send(());
            })
            .context("spawn native user reader")?;
        Ok(Self {
            stop,
            done,
            thread: Some(thread),
        })
    }
}
impl Drop for RolloutReader {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if self.done.recv_timeout(Duration::from_secs(1)).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
