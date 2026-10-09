//! Bounded channel receipts, never native commands or message bodies.
//!
//! A caller must persist `Submitting` before its one native RPC attempt. Recovery
//! reports uncertainty; it does not restore a binding, enqueue input, or retry it.
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::telegram::{TOKEN_LIMIT, valid_token};
use crate::workbench::recording::fs::{Directory, Identity};

const JOURNAL: &str = "receipts.jsonl";
const LOCK: &str = "writer.lock";
const TOKEN: &str = "telegram.token";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_TASKS: usize = 4096;
const MAX_LINE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryKey {
    pub bot_id: i64,
    pub chat_id: i64,
    pub message_id: i64,
}
impl DeliveryKey {
    fn valid(self) -> bool {
        self.bot_id > 0 && self.chat_id > 0 && self.message_id > 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Fresh,
    Duplicate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Received,
    Submitting,
    /// Receipt closed; does not prove native completion or Telegram delivery.
    Closed,
}
/// Static diagnostics only: never include OS errors, paths, JSON, or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    InvalidInput,
    InvalidTransition,
    Busy,
    Unavailable,
    Corrupt,
    Capacity,
    Poisoned,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "channel store invalid input",
            Self::InvalidTransition => "channel store invalid transition",
            Self::Busy => "channel store already in use",
            Self::Unavailable => "channel store unavailable",
            Self::Corrupt => "channel store integrity failure",
            Self::Capacity => "channel store capacity reached",
            Self::Poisoned => "channel store disabled after failure",
        })
    }
}
impl std::error::Error for Failure {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    sequence: u64,
    event: Event,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Rejected {
        bot_id: i64,
        update_id: i64,
        message_key: Option<DeliveryKey>,
    },
    Received {
        key: DeliveryKey,
        update_id: i64,
        task: Uuid,
    },
    Submitting {
        task: Uuid,
        client_id: Uuid,
    },
    Closed {
        task: Uuid,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    record: Record,
    digest: String,
}

// Intentionally no Debug on storage or serialized records.
pub struct Store {
    dir: Directory,
    lock: File,
    lock_identity: Identity,
    journal: File,
    journal_identity: Identity,
    keys: HashSet<DeliveryKey>,
    rejected_updates: HashSet<(i64, i64)>,
    rejected_keys: HashSet<DeliveryKey>,
    tasks: BTreeMap<Uuid, Status>,
    clients: HashSet<Uuid>,
    sequence: u64,
    bytes: usize,
    poisoned: bool,
}

impl Store {
    /// The parent must already exist and be owned/private (0700). Creating the
    /// root is descriptor-relative and syncs its parent before accepting input.
    /// A missing journal beside an existing lock is an integrity error, including
    /// an interrupted first initialization.
    pub fn open(root: &Path) -> Result<Self, Failure> {
        let parent_path = root.parent().ok_or(Failure::InvalidInput)?;
        let parent_path = if parent_path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent_path
        };
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Failure::InvalidInput)?;
        let parent = Directory::read_only(parent_path).map_err(|_| Failure::Unavailable)?;
        parent.verify_location().map_err(|_| Failure::Unavailable)?;
        let dir = parent.dir(name, true).map_err(|_| Failure::Unavailable)?;
        parent.verify_location().map_err(|_| Failure::Unavailable)?;
        dir.verify_location().map_err(|_| Failure::Unavailable)?;
        let (lock, created) = match dir.open(LOCK, true) {
            Ok(file) => (file, true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
                dir.open(LOCK, false).map_err(|_| Failure::Unavailable)?,
                false,
            ),
            Err(_) => return Err(Failure::Unavailable),
        };
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            return Err(Failure::Busy);
        }
        let lock_identity = identity(&lock)?;
        if created {
            // Refuse to replace a deleted lock for an existing journal.
            match dir.entry_info(JOURNAL) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(Failure::Corrupt),
            }
            lock.sync_all().map_err(|_| Failure::Unavailable)?;
            let file = dir.open(JOURNAL, true).map_err(|_| Failure::Unavailable)?;
            file.sync_all().map_err(|_| Failure::Unavailable)?;
            dir.sync().map_err(|_| Failure::Unavailable)?;
        }
        let journal = dir.append(JOURNAL).map_err(|_| Failure::Unavailable)?;
        let journal_identity = identity(&journal)?;
        let mut store = Self {
            dir,
            lock,
            lock_identity,
            journal,
            journal_identity,
            keys: HashSet::new(),
            rejected_updates: HashSet::new(),
            rejected_keys: HashSet::new(),
            tasks: BTreeMap::new(),
            clients: HashSet::new(),
            sequence: 0,
            bytes: 0,
            poisoned: false,
        };
        let len = store
            .journal
            .metadata()
            .map_err(|_| Failure::Unavailable)?
            .len();
        if len > MAX_BYTES as u64 {
            return Err(Failure::Capacity);
        }
        store.bytes = len as usize;
        store.verify()?;
        let mut bytes = Vec::new();
        (&store.journal)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Unavailable)?;
        if bytes.len() != store.bytes {
            return Err(Failure::Corrupt);
        }
        if !bytes.is_empty()
            && (!bytes.ends_with(b"\n")
                || bytes.starts_with(b"\n")
                || bytes.windows(2).any(|pair| pair == b"\n\n"))
        {
            return Err(Failure::Corrupt);
        }
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if line.is_empty() || line.len() > MAX_LINE {
                return Err(Failure::Corrupt);
            }
            let line: Line = serde_json::from_slice(line).map_err(|_| Failure::Corrupt)?;
            let encoded = serde_json::to_vec(&line.record).map_err(|_| Failure::Corrupt)?;
            if !matches!(line.record.version, 1 | 2)
                || (line.record.version == 1 && matches!(line.record.event, Event::Rejected { .. }))
                || line.record.sequence != store.sequence + 1
                || line.digest != blake3::hash(&encoded).to_hex().as_str()
            {
                return Err(Failure::Corrupt);
            }
            store
                .validate(&line.record.event)
                .map_err(|_| Failure::Corrupt)?;
            store.apply(line.record.event);
            store.sequence += 1;
        }
        store.journal.sync_all().map_err(|_| Failure::Unavailable)?;
        store.dir.sync().map_err(|_| Failure::Unavailable)?;
        store.verify()?;
        Ok(store)
    }

    pub fn admit(
        &mut self,
        key: DeliveryKey,
        update_id: i64,
        task: Uuid,
    ) -> Result<Admission, Failure> {
        self.verify()?;
        if !key.valid() || update_id < 0 || task.is_nil() {
            return Err(Failure::InvalidInput);
        }
        // Message identity survives update redelivery and binding generations.
        if self.keys.contains(&key) || self.rejected(key.bot_id, update_id, Some(key)) {
            return Ok(Admission::Duplicate);
        }
        self.append(Event::Received {
            key,
            update_id,
            task,
        })?;
        Ok(Admission::Fresh)
    }
    /// Exact rejection identities survive offset resets, reconfiguration and
    /// restart. No monotonic Telegram update-ID assumption is required.
    pub fn is_rejected(
        &mut self,
        bot_id: i64,
        update_id: i64,
        key: Option<DeliveryKey>,
    ) -> Result<bool, Failure> {
        self.verify()?;
        validate_rejection(bot_id, update_id, key)?;
        Ok(self.rejected(bot_id, update_id, key))
    }
    /// Returns only after the rejection is durable; duplicates never append.
    /// The caller may provide a key only for a validated positive private-chat
    /// message identity. The journal contains no user identity or message body.
    pub fn reject(
        &mut self,
        bot_id: i64,
        update_id: i64,
        key: Option<DeliveryKey>,
    ) -> Result<(), Failure> {
        self.verify()?;
        validate_rejection(bot_id, update_id, key)?;
        if self.rejected(bot_id, update_id, key) {
            return Ok(());
        }
        self.append(Event::Rejected {
            bot_id,
            update_id,
            message_key: key,
        })
    }
    fn rejected(&self, bot_id: i64, update_id: i64, key: Option<DeliveryKey>) -> bool {
        self.rejected_updates.contains(&(bot_id, update_id))
            || key.is_some_and(|key| self.rejected_keys.contains(&key))
    }
    pub fn mark_submitting(&mut self, task: Uuid, client_id: Uuid) -> Result<(), Failure> {
        self.append(Event::Submitting { task, client_id })
    }
    /// Also permits abandoning a received message without ever submitting it.
    pub fn close(&mut self, task: Uuid) -> Result<(), Failure> {
        self.append(Event::Closed { task })
    }
    pub fn recover(&mut self) -> Result<Vec<(Uuid, Status)>, Failure> {
        self.verify()?;
        Ok(self
            .tasks
            .iter()
            .map(|(task, status)| (*task, *status))
            .collect())
    }

    pub fn token(&mut self) -> Result<Option<String>, Failure> {
        self.verify()?;
        match self.dir.read(TOKEN, TOKEN_LIMIT) {
            Ok(bytes) => {
                let token = String::from_utf8(bytes).map_err(|_| Failure::Corrupt)?;
                if !valid_token(&token) {
                    return Err(Failure::Corrupt);
                }
                self.verify()?;
                Ok(Some(token))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Failure::Unavailable),
        }
    }
    pub fn save_token(&mut self, token: &str) -> Result<(), Failure> {
        self.verify()?;
        if !valid_token(token) {
            return Err(Failure::InvalidInput);
        }
        check_optional_file(&self.dir, TOKEN)?;
        self.dir
            .atomic_when(TOKEN, token.as_bytes(), || {
                self.dir.verify_location().is_ok()
                    && self.verify_files().is_ok()
                    && check_optional_file(&self.dir, TOKEN).is_ok()
            })
            .map_err(|_| Failure::Unavailable)?;
        self.verify()?;
        Ok(())
    }
    pub fn remove_token(&mut self) -> Result<(), Failure> {
        self.verify()?;
        check_optional_file(&self.dir, TOKEN)?;
        match self.dir.remove(TOKEN) {
            Ok(()) => self.dir.sync().map_err(|_| Failure::Unavailable)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(Failure::Unavailable),
        }
        self.verify()
    }

    fn verify_files(&self) -> Result<(), Failure> {
        for (name, file, expected) in [
            (LOCK, &self.lock, self.lock_identity),
            (JOURNAL, &self.journal, self.journal_identity),
        ] {
            let info = self
                .dir
                .entry_info(name)
                .map_err(|_| Failure::Unavailable)?;
            if info.directory || info.identity != expected || identity(file)? != expected {
                return Err(Failure::Corrupt);
            }
            if name == JOURNAL && info.bytes != self.bytes as u64 {
                return Err(Failure::Corrupt);
            }
        }
        Ok(())
    }
    fn verify(&mut self) -> Result<(), Failure> {
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
        let result = self
            .dir
            .verify_location()
            .map_err(|_| Failure::Unavailable)
            .and_then(|()| self.verify_files());
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn validate(&self, event: &Event) -> Result<(), Failure> {
        match event {
            Event::Rejected {
                bot_id,
                update_id,
                message_key,
            } => {
                validate_rejection(*bot_id, *update_id, *message_key)?;
                if self.rejected(*bot_id, *update_id, *message_key) {
                    return Err(Failure::InvalidTransition);
                }
                if self.tasks.len() + self.rejected_updates.len() >= MAX_TASKS {
                    return Err(Failure::Capacity);
                }
            }
            Event::Received {
                key,
                update_id,
                task,
            } => {
                if !key.valid() || *update_id < 0 || task.is_nil() {
                    return Err(Failure::InvalidInput);
                }
                if self.keys.contains(key)
                    || self.tasks.contains_key(task)
                    || self.rejected(key.bot_id, *update_id, Some(*key))
                {
                    return Err(Failure::InvalidTransition);
                }
                if self.tasks.len() + self.rejected_updates.len() >= MAX_TASKS {
                    return Err(Failure::Capacity);
                }
            }
            Event::Submitting { task, client_id } => {
                if client_id.is_nil() {
                    return Err(Failure::InvalidInput);
                }
                if self.tasks.get(task) != Some(&Status::Received)
                    || self.clients.contains(client_id)
                {
                    return Err(Failure::InvalidTransition);
                }
            }
            Event::Closed { task } => {
                if !matches!(
                    self.tasks.get(task),
                    Some(Status::Received | Status::Submitting)
                ) {
                    return Err(Failure::InvalidTransition);
                }
            }
        }
        Ok(())
    }
    fn apply(&mut self, event: Event) {
        match event {
            Event::Rejected {
                bot_id,
                update_id,
                message_key,
            } => {
                self.rejected_updates.insert((bot_id, update_id));
                if let Some(key) = message_key {
                    self.rejected_keys.insert(key);
                }
            }
            Event::Received { key, task, .. } => {
                self.keys.insert(key);
                self.tasks.insert(task, Status::Received);
            }
            Event::Submitting { task, client_id } => {
                self.clients.insert(client_id);
                self.tasks.insert(task, Status::Submitting);
            }
            Event::Closed { task } => {
                self.tasks.insert(task, Status::Closed);
            }
        }
    }
    fn append(&mut self, event: Event) -> Result<(), Failure> {
        self.verify()?;
        self.validate(&event)?;
        let record = Record {
            // v1 readers deliberately fail closed rather than silently dropping
            // rejection records. Existing v1 records remain readable above.
            version: 2,
            sequence: self.sequence + 1,
            event,
        };
        let encoded = serde_json::to_vec(&record).map_err(|_| Failure::Corrupt)?;
        let line = Line {
            record,
            digest: blake3::hash(&encoded).to_hex().to_string(),
        };
        let mut bytes = serde_json::to_vec(&line).map_err(|_| Failure::Corrupt)?;
        if bytes.len() > MAX_LINE || self.bytes + bytes.len() + 1 > MAX_BYTES {
            return Err(Failure::Capacity);
        }
        bytes.push(b'\n');
        // Any write/fsync failure may have left an uncertain suffix. Never retry
        // on this handle, even if a later storage operation might succeed.
        self.poisoned = true;
        self.journal
            .write_all(&bytes)
            .map_err(|_| Failure::Unavailable)?;
        self.journal.sync_all().map_err(|_| Failure::Unavailable)?;
        self.dir.sync().map_err(|_| Failure::Unavailable)?;
        self.bytes += bytes.len();
        self.dir
            .verify_location()
            .map_err(|_| Failure::Unavailable)?;
        self.verify_files()?;
        self.sequence += 1;
        self.apply(line.record.event);
        self.poisoned = false;
        Ok(())
    }
}

fn validate_rejection(
    bot_id: i64,
    update_id: i64,
    key: Option<DeliveryKey>,
) -> Result<(), Failure> {
    if bot_id <= 0 || update_id < 0 || key.is_some_and(|key| !key.valid() || key.bot_id != bot_id) {
        Err(Failure::InvalidInput)
    } else {
        Ok(())
    }
}

fn identity(file: &File) -> Result<Identity, Failure> {
    let meta = file.metadata().map_err(|_| Failure::Unavailable)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err(Failure::Unavailable);
    }
    Ok(Identity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}
fn check_optional_file(dir: &Directory, name: &str) -> Result<(), Failure> {
    match dir.entry_info(name) {
        Ok(info) if !info.directory => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(Failure::Unavailable),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn key(message_id: i64) -> DeliveryKey {
        DeliveryKey {
            bot_id: 123,
            chat_id: 456,
            message_id,
        }
    }
    fn setup() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::open(&dir.path().join("channel")).unwrap();
        (dir, store)
    }
    fn assert_reopen_fails(path: &Path) {
        assert!(Store::open(path).is_err());
    }

    #[test]
    fn rejected_metadata_survives_restart_without_becoming_a_task() {
        let (dir, mut store) = setup();
        store.reject(123, 7, Some(key(9))).unwrap();
        store.reject(123, 8, None).unwrap();
        let bytes = store.bytes;
        store.reject(123, 7, Some(key(9))).unwrap();
        store.reject(123, 999, Some(key(9))).unwrap();
        assert_eq!(store.bytes, bytes);
        assert!(store.recover().unwrap().is_empty());
        drop(store);
        let mut store = Store::open(&dir.path().join("channel")).unwrap();
        assert!(store.is_rejected(123, 7, None).unwrap());
        assert!(store.is_rejected(123, 8, None).unwrap());
        assert!(store.is_rejected(123, 999, Some(key(9))).unwrap());
        assert!(!store.is_rejected(124, 7, None).unwrap());
        assert_eq!(
            store.admit(key(10), 8, Uuid::from_u128(1)),
            Ok(Admission::Duplicate)
        );
        assert_eq!(
            store.admit(key(9), 999, Uuid::from_u128(2)),
            Ok(Admission::Duplicate)
        );
        assert!(store.recover().unwrap().is_empty());
        let text = fs::read_to_string(dir.path().join("channel").join(JOURNAL)).unwrap();
        assert!(!text.contains("task"));
        assert!(!text.contains("user_id"));
        for line in text.lines() {
            let line: Line = serde_json::from_str(line).unwrap();
            assert_eq!(line.record.version, 2);
            // The previous reader's exact version gate rejects every new row.
            assert_ne!(line.record.version, 1);
        }
    }

    fn fixture_line(record: Record) -> Vec<u8> {
        let digest = blake3::hash(&serde_json::to_vec(&record).unwrap())
            .to_hex()
            .to_string();
        let mut bytes = serde_json::to_vec(&Line { record, digest }).unwrap();
        bytes.push(b'\n');
        bytes
    }
    #[test]
    fn v1_receipts_remain_readable_but_cannot_claim_rejection_schema() {
        let (dir, store) = setup();
        let path = dir.path().join("channel");
        drop(store);
        fs::write(
            path.join(JOURNAL),
            fixture_line(Record {
                version: 1,
                sequence: 1,
                event: Event::Received {
                    key: key(1),
                    update_id: 1,
                    task: Uuid::from_u128(1),
                },
            }),
        )
        .unwrap();
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store.admit(key(1), 2, Uuid::from_u128(2)),
            Ok(Admission::Duplicate)
        );
        store.reject(123, 3, Some(key(3))).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(store.is_rejected(123, 3, Some(key(3))).unwrap());
        drop(store);
        for version in [1, 3] {
            fs::write(
                path.join(JOURNAL),
                fixture_line(Record {
                    version,
                    sequence: 1,
                    event: Event::Rejected {
                        bot_id: 123,
                        update_id: 1,
                        message_key: Some(key(1)),
                    },
                }),
            )
            .unwrap();
            assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
        }
    }

    #[test]
    fn rejected_identity_validation_and_combined_cap_fail_closed() {
        let (_dir, mut store) = setup();
        for (bot, update, key) in [
            (0, 1, None),
            (123, -1, None),
            (124, 1, Some(key(1))),
            (123, 1, Some(key(0))),
        ] {
            assert_eq!(store.reject(bot, update, key), Err(Failure::InvalidInput));
            assert_eq!(
                store.is_rejected(bot, update, key),
                Err(Failure::InvalidInput)
            );
        }
        // Exercise the actual combined boundary without thousands of redundant
        // disk flushes; persistence/reopen is covered by the preceding test.
        for id in 0..MAX_TASKS - 1 {
            store.rejected_updates.insert((123, id as i64));
        }
        assert_eq!(
            store.admit(key(1), MAX_TASKS as i64, Uuid::from_u128(1)),
            Ok(Admission::Fresh)
        );
        assert_eq!(
            store.reject(123, MAX_TASKS as i64 + 1, None),
            Err(Failure::Capacity)
        );
        assert_eq!(
            store.admit(key(2), MAX_TASKS as i64 + 2, Uuid::from_u128(2)),
            Err(Failure::Capacity)
        );
        assert_eq!(store.reject(123, 0, None), Ok(()));
        assert_eq!(
            store.admit(key(2), 0, Uuid::from_u128(2)),
            Ok(Admission::Duplicate)
        );
    }

    #[test]
    fn receipts_survive_reopen_and_deduplicate_message_not_update() {
        let (dir, mut store) = setup();
        let task = Uuid::new_v4();
        assert_eq!(store.admit(key(1), 20, task), Ok(Admission::Fresh));
        assert_eq!(
            store.admit(key(1), 21, Uuid::new_v4()),
            Ok(Admission::Duplicate)
        );
        store.mark_submitting(task, Uuid::new_v4()).unwrap();
        drop(store);
        let mut store = Store::open(&dir.path().join("channel")).unwrap();
        assert_eq!(store.recover().unwrap(), vec![(task, Status::Submitting)]);
        assert_eq!(
            store.admit(key(1), 99, Uuid::new_v4()),
            Ok(Admission::Duplicate)
        );
        // Recovery reports the uncertain attempt and cannot re-submit it.
        assert_eq!(
            store.mark_submitting(task, Uuid::new_v4()),
            Err(Failure::InvalidTransition)
        );
        store.close(task).unwrap();
        drop(store);
        let mut store = Store::open(&dir.path().join("channel")).unwrap();
        assert_eq!(store.recover().unwrap(), vec![(task, Status::Closed)]);
        assert_eq!(
            store.admit(key(1), 100, Uuid::new_v4()),
            Ok(Admission::Duplicate)
        );
    }
    const CRASH_TASK: Uuid = Uuid::from_u128(0xa11ce);
    const CRASH_CLIENT: Uuid = Uuid::from_u128(0xc11e17);
    const CRASH_EXIT: i32 = 73;

    #[test]
    fn durable_submission_crash_child() {
        // Ordinary suite execution is inert. Only the isolated parent below
        // sets this marker and selects this exact helper in a new process.
        if std::env::var("CODEX_CHANNEL_STORE_CRASH_CHILD").as_deref() != Ok("1") {
            return;
        }
        let root = std::env::var_os("CODEX_CHANNEL_STORE_CRASH_ROOT")
            .expect("synthetic crash root must be configured");
        let mut store = Store::open(Path::new(&root)).unwrap();
        assert_eq!(store.admit(key(7), 19, CRASH_TASK), Ok(Admission::Fresh));
        store.mark_submitting(CRASH_TASK, CRASH_CLIENT).unwrap();
        store.reject(123, 21, Some(key(8))).unwrap();
        // Both APIs have fsynced. Exit immediately without unwinding or running
        // Store/File destructors, as opposed to merely dropping/reopening it.
        std::process::exit(CRASH_EXIT);
    }

    #[test]
    fn durable_submission_survives_abrupt_process_exit_without_replay() {
        let sandbox = tempfile::tempdir().unwrap();
        fs::set_permissions(sandbox.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["home", "codex-home", "tmp"] {
            let path = sandbox.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let root = sandbox.path().join("channel");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workbench::channel::store::tests::durable_submission_crash_child",
                "--test-threads=1",
            ])
            .env_clear()
            .env("HOME", sandbox.path().join("home"))
            .env("USERPROFILE", sandbox.path().join("home"))
            .env("CODEX_HOME", sandbox.path().join("codex-home"))
            .env("TMPDIR", sandbox.path().join("tmp"))
            .env("CODEX_CHANNEL_STORE_CRASH_CHILD", "1")
            .env("CODEX_CHANNEL_STORE_CRASH_ROOT", &root)
            .current_dir(sandbox.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("synthetic crash child must run");
        assert_eq!(status.code(), Some(CRASH_EXIT));
        let before = fs::read(root.join(JOURNAL)).unwrap();
        let mut store = Store::open(&root).unwrap();
        assert!(store.is_rejected(123, 21, Some(key(8))).unwrap());
        assert_eq!(
            store.admit(key(8), 22, Uuid::from_u128(0xa11d0)),
            Ok(Admission::Duplicate)
        );
        assert_eq!(
            store.recover().unwrap(),
            vec![(CRASH_TASK, Status::Submitting)]
        );
        assert_eq!(
            store.admit(key(7), 20, Uuid::from_u128(0xa11cf)),
            Ok(Admission::Duplicate)
        );
        assert_eq!(
            store.mark_submitting(CRASH_TASK, CRASH_CLIENT),
            Err(Failure::InvalidTransition)
        );
        assert_eq!(
            store.recover().unwrap(),
            vec![(CRASH_TASK, Status::Submitting)]
        );
        assert_eq!(fs::read(root.join(JOURNAL)).unwrap(), before);
    }

    #[test]
    fn validates_input_transitions_and_client_uniqueness() {
        let (_dir, mut store) = setup();
        let task = Uuid::new_v4();
        assert_eq!(store.admit(key(0), 0, task), Err(Failure::InvalidInput));
        assert_eq!(store.admit(key(1), -1, task), Err(Failure::InvalidInput));
        assert_eq!(
            store.admit(key(1), 0, Uuid::nil()),
            Err(Failure::InvalidInput)
        );
        assert_eq!(store.close(task), Err(Failure::InvalidTransition));
        store.admit(key(1), 0, task).unwrap();
        assert_eq!(
            store.admit(key(2), 1, task),
            Err(Failure::InvalidTransition)
        );
        let client = Uuid::new_v4();
        store.mark_submitting(task, client).unwrap();
        let second = Uuid::new_v4();
        store.admit(key(2), 1, second).unwrap();
        assert_eq!(
            store.mark_submitting(second, client),
            Err(Failure::InvalidTransition)
        );
        store.close(second).unwrap();
        assert_eq!(store.close(second), Err(Failure::InvalidTransition));
    }
    #[test]
    fn lock_is_exclusive_and_files_are_private() {
        let (dir, store) = setup();
        let root = dir.path().join("channel");
        assert!(matches!(Store::open(&root), Err(Failure::Busy)));
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        for name in [LOCK, JOURNAL] {
            assert_eq!(fs::metadata(root.join(name)).unwrap().mode() & 0o777, 0o600);
        }
        drop(store);
        assert!(Store::open(&root).is_ok());
    }
    #[test]
    fn torn_corrupt_unknown_and_out_of_order_lines_fail_closed() {
        for corruption in 0..6 {
            let (dir, mut store) = setup();
            store.admit(key(1), 1, Uuid::new_v4()).unwrap();
            drop(store);
            let path = dir.path().join("channel").join(JOURNAL);
            let mut bytes = fs::read(&path).unwrap();
            match corruption {
                0 => {
                    bytes.pop();
                }
                1 => {
                    bytes.extend_from_slice(b"{\"record\":");
                }
                2 => {
                    bytes.extend_from_slice(b"\n");
                }
                3 => {
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    value["unexpected"] = true.into();
                    bytes = serde_json::to_vec(&value).unwrap();
                    bytes.push(b'\n');
                }
                4 => {
                    let copy = bytes.clone();
                    bytes.extend(copy);
                }
                _ => {
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    value["record"]["event"]["key"]["chat_id"] = 987.into();
                    bytes = serde_json::to_vec(&value).unwrap();
                    bytes.push(b'\n');
                }
            }
            fs::write(&path, bytes).unwrap();
            assert_reopen_fails(&dir.path().join("channel"));
        }
    }
    #[test]
    fn replacement_or_truncation_permanently_disables_open_store() {
        for name in [JOURNAL, LOCK, "root", "truncate"] {
            let (dir, mut store) = setup();
            store.admit(key(1), 1, Uuid::new_v4()).unwrap();
            let root = dir.path().join("channel");
            if name == "root" {
                fs::rename(&root, dir.path().join("old")).unwrap();
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            } else if name == "truncate" {
                fs::write(root.join(JOURNAL), b"").unwrap();
            } else {
                fs::rename(root.join(name), root.join("old")).unwrap();
                fs::write(root.join(name), b"").unwrap();
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(store.admit(key(2), 2, Uuid::new_v4()).is_err());
            assert_eq!(
                store.admit(key(3), 3, Uuid::new_v4()),
                Err(Failure::Poisoned)
            );
        }
    }
    #[test]
    fn journal_lock_and_token_reject_symlinks_hardlinks_and_public_modes() {
        for name in [JOURNAL, LOCK, TOKEN] {
            for attack in ["symlink", "hardlink", "permissions"] {
                let (dir, mut store) = setup();
                store.save_token("123:synthetic_only").unwrap();
                let root = dir.path().join("channel");
                drop(store);
                let target = root.join(name);
                if attack == "permissions" {
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
                } else {
                    let outside = dir.path().join("outside");
                    fs::rename(&target, &outside).unwrap();
                    if attack == "symlink" {
                        symlink(&outside, &target).unwrap();
                    } else {
                        fs::hard_link(&outside, &target).unwrap();
                    }
                }
                if name == TOKEN {
                    let mut reopened = Store::open(&root).unwrap();
                    assert!(reopened.token().is_err());
                    assert!(reopened.save_token("123:replacement").is_err());
                    assert!(reopened.remove_token().is_err());
                } else {
                    assert_reopen_fails(&root);
                }
            }
        }
    }
    #[test]
    fn root_symlinks_and_public_directory_are_rejected() {
        let (dir, store) = setup();
        drop(store);
        symlink(dir.path().join("channel"), dir.path().join("link")).unwrap();
        assert_reopen_fails(&dir.path().join("link"));
        fs::set_permissions(
            dir.path().join("channel"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_reopen_fails(&dir.path().join("channel"));
    }
    #[test]
    fn root_creation_requires_a_private_parent() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_reopen_fails(&dir.path().join("channel"));
        assert!(!dir.path().join("channel").exists());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(Store::open(&dir.path().join("channel")).is_ok());
    }
    #[test]
    fn deleted_journal_and_interrupted_initialization_never_reset_receipts() {
        let (dir, store) = setup();
        drop(store);
        fs::remove_file(dir.path().join("channel").join(JOURNAL)).unwrap();
        assert_reopen_fails(&dir.path().join("channel"));
        let fresh = dir.path().join("incomplete");
        let directory = Directory::root(&fresh).unwrap();
        directory.open(LOCK, true).unwrap();
        assert_reopen_fails(&fresh);
    }
    #[test]
    fn append_io_failure_poisoning_prevents_later_admission() {
        let (_dir, mut store) = setup();
        // A read-only descriptor deterministically injects an append failure,
        // with the same valid inode and without touching real user storage.
        store.journal = store.dir.open(JOURNAL, false).unwrap();
        assert_eq!(
            store.admit(key(1), 1, Uuid::new_v4()),
            Err(Failure::Unavailable)
        );
        assert_eq!(
            store.admit(key(2), 2, Uuid::new_v4()),
            Err(Failure::Poisoned)
        );
    }
    #[test]
    fn bounded_task_and_byte_capacity_do_not_evict_receipts() {
        let (_dir, mut store) = setup();
        // Build a valid synthetic journal in one fsync to keep the capacity test
        // fast; reopen still validates every receipt and its digest.
        let mut bytes = Vec::new();
        for index in 1..=MAX_TASKS {
            let record = Record {
                version: 1,
                sequence: index as u64,
                event: Event::Received {
                    key: key(index as i64),
                    update_id: index as i64,
                    task: Uuid::new_v4(),
                },
            };
            let digest = blake3::hash(&serde_json::to_vec(&record).unwrap())
                .to_hex()
                .to_string();
            bytes.extend(serde_json::to_vec(&Line { record, digest }).unwrap());
            bytes.push(b'\n');
        }
        store.journal.write_all(&bytes).unwrap();
        store.journal.sync_all().unwrap();
        let path = store.dir.path.clone();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store.admit(key(MAX_TASKS as i64 + 1), 9999, Uuid::new_v4()),
            Err(Failure::Capacity)
        );
        assert_eq!(
            store.admit(key(1), 9999, Uuid::new_v4()),
            Ok(Admission::Duplicate)
        );
        drop(store);
        let directory = Directory::read_only(&path).unwrap();
        directory
            .append(JOURNAL)
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        assert!(matches!(Store::open(&path), Err(Failure::Capacity)));
    }
    #[test]
    fn credentials_are_separate_atomic_private_and_errors_are_static() {
        let (dir, mut store) = setup();
        assert!(store.token().unwrap().is_none());
        store.save_token("123:synthetic_only-A").unwrap();
        let path = dir.path().join("channel").join(TOKEN);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let first = fs::metadata(&path).unwrap().ino();
        store.save_token("123:synthetic_only-B").unwrap();
        assert_ne!(first, fs::metadata(&path).unwrap().ino());
        drop(store);
        let mut store = Store::open(&dir.path().join("channel")).unwrap();
        assert_eq!(
            store.token().unwrap().as_deref(),
            Some("123:synthetic_only-B")
        );
        assert!(
            fs::read(dir.path().join("channel").join(JOURNAL))
                .unwrap()
                .is_empty()
        );
        for invalid in [
            "",
            "0:secret",
            "123:",
            "123:secret\n",
            "https://token",
            "123:非ASCII",
            "9223372036854775808:secret",
            &format!("123:{}", "a".repeat(TOKEN_LIMIT)),
        ] {
            let error = store.save_token(invalid).unwrap_err();
            assert_eq!(error, Failure::InvalidInput);
            assert_eq!(error.to_string(), "channel store invalid input");
        }
        fs::write(&path, [0xff]).unwrap();
        assert_eq!(store.token(), Err(Failure::Corrupt));
        store.save_token("123:synthetic_repaired").unwrap();
        fs::write(&path, vec![b'a'; TOKEN_LIMIT + 1]).unwrap();
        assert_eq!(store.token(), Err(Failure::Unavailable));
        store.remove_token().unwrap();
        store.remove_token().unwrap();
        assert!(store.token().unwrap().is_none());
        assert_eq!(fs::read_dir(dir.path().join("channel")).unwrap().count(), 2);
    }
}
