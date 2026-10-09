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
const MAX_QUERY: usize = 1024;

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
    CallbackReceived {
        bot_id: i64,
        update_id: i64,
        query_digest: String,
        task: Uuid,
        interaction: Uuid,
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
    task_bots: BTreeMap<Uuid, i64>,
    received_updates: HashSet<(i64, i64)>,
    callback_updates: HashSet<(i64, i64)>,
    callback_queries: HashSet<(i64, String)>,
    clients: HashSet<Uuid>,
    sequence: u64,
    bytes: usize,
    poisoned: bool,
    #[cfg(test)]
    fail_next_sync: bool,
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
            task_bots: BTreeMap::new(),
            received_updates: HashSet::new(),
            callback_updates: HashSet::new(),
            callback_queries: HashSet::new(),
            clients: HashSet::new(),
            sequence: 0,
            bytes: 0,
            poisoned: false,
            #[cfg(test)]
            fail_next_sync: false,
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
            if !matches!(line.record.version, 1..=3)
                || (line.record.version == 1 && matches!(line.record.event, Event::Rejected { .. }))
                || (line.record.version < 3
                    && matches!(line.record.event, Event::CallbackReceived { .. }))
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
        if self.keys.contains(&key)
            || self.callback_updates.contains(&(key.bot_id, update_id))
            || self.rejected(key.bot_id, update_id, Some(key))
        {
            return Ok(Admission::Duplicate);
        }
        self.append(Event::Received {
            key,
            update_id,
            task,
        })?;
        Ok(Admission::Fresh)
    }
    /// This durable receipt permits only the current actor's one volatile
    /// attempt, after it revalidates its binding and interaction. Recovery never
    /// reconstructs a callback, action, native request, or executable command.
    pub fn admit_callback(
        &mut self,
        bot_id: i64,
        update_id: i64,
        query_id: &str,
        task: Uuid,
        interaction: Uuid,
    ) -> Result<Admission, Failure> {
        self.verify()?;
        validate_rejection(bot_id, update_id, None)?;
        if !valid_query_id(query_id) || task.is_nil() || interaction.is_nil() {
            return Err(Failure::InvalidInput);
        }
        let query_digest = blake3::hash(query_id.as_bytes()).to_hex().to_string();
        // Dedup precedes current task status/bot checks: a retired binding or
        // closed task cannot turn an already consumed identity into fresh work.
        // A duplicate never associates its other, possibly conflicting identity.
        if self.callback_seen(bot_id, update_id, &query_digest) {
            return Ok(Admission::Duplicate);
        }
        self.append(Event::CallbackReceived {
            bot_id,
            update_id,
            query_digest,
            task,
            interaction,
        })?;
        Ok(Admission::Fresh)
    }
    fn callback_seen(&self, bot_id: i64, update_id: i64, query_digest: &str) -> bool {
        self.rejected_updates.contains(&(bot_id, update_id))
            || self.received_updates.contains(&(bot_id, update_id))
            || self.callback_updates.contains(&(bot_id, update_id))
            || self
                .callback_queries
                .contains(&(bot_id, query_digest.to_owned()))
    }
    fn receipt_count(&self) -> usize {
        self.tasks.len() + self.rejected_updates.len() + self.callback_updates.len()
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
        if self.rejected(bot_id, update_id, key)
            || self.callback_updates.contains(&(bot_id, update_id))
        {
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
                if self.rejected(*bot_id, *update_id, *message_key)
                    || self.callback_updates.contains(&(*bot_id, *update_id))
                {
                    return Err(Failure::InvalidTransition);
                }
                if self.receipt_count() >= MAX_TASKS {
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
                    || self.callback_updates.contains(&(key.bot_id, *update_id))
                    || self.tasks.contains_key(task)
                    || self.rejected(key.bot_id, *update_id, Some(*key))
                {
                    return Err(Failure::InvalidTransition);
                }
                if self.receipt_count() >= MAX_TASKS {
                    return Err(Failure::Capacity);
                }
            }
            Event::CallbackReceived {
                bot_id,
                update_id,
                query_digest,
                task,
                interaction,
            } => {
                validate_rejection(*bot_id, *update_id, None)?;
                if task.is_nil()
                    || interaction.is_nil()
                    || query_digest.len() != 64
                    || !query_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(Failure::InvalidInput);
                }
                if self.callback_seen(*bot_id, *update_id, query_digest)
                    || self.tasks.get(task) != Some(&Status::Submitting)
                    || self.task_bots.get(task) != Some(bot_id)
                {
                    return Err(Failure::InvalidTransition);
                }
                if self.receipt_count() >= MAX_TASKS {
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
            Event::Received {
                key,
                update_id,
                task,
            } => {
                self.keys.insert(key);
                self.task_bots.insert(task, key.bot_id);
                self.received_updates.insert((key.bot_id, update_id));
                self.tasks.insert(task, Status::Received);
            }
            Event::CallbackReceived {
                bot_id,
                update_id,
                query_digest,
                ..
            } => {
                self.callback_updates.insert((bot_id, update_id));
                self.callback_queries.insert((bot_id, query_digest));
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
            // Older readers fail closed on new metadata. Existing v1/v2
            // rows remain readable in a mixed, continuously sequenced journal.
            version: 3,
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
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_sync) {
            return Err(Failure::Unavailable);
        }
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

pub(super) fn valid_query_id(query_id: &str) -> bool {
    !query_id.is_empty() && query_id.len() <= MAX_QUERY && !query_id.chars().any(char::is_control)
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
#[path = "store/tests.rs"]
mod tests;
