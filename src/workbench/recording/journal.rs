use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::fs::{Directory, FILE_LIMIT};
use super::replay::{Checkpoint, Document};

pub(super) const FORMAT: u32 = 1;
const LINE_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gap {
    pub after_view_seq: u64,
    pub through_view_seq: u64,
    pub after_record_seq: u64,
    pub through_record_seq: u64,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Segment {
    pub id: Uuid,
    pub base: String,
    pub bytes: u64,
    pub record_seq: u64,
    pub view_seq: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Meta {
    pub format_version: u32,
    pub run_epoch: Uuid,
    pub workspace_id: String,
    pub project_name: String,
    pub started_at: String,
    pub ended: bool,
    pub persisted_through_view_seq: u64,
    pub saved_through_view_seq: u64,
    pub saved_record_seq: u64,
    pub segments: Vec<Segment>,
    pub gaps: Vec<Gap>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    format_version: u32,
    run_epoch: Uuid,
    record_seq: u64,
    view_seq: u64,
    kind: String,
    data: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    record: Record,
    digest: String,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn encode(value: &impl Serialize) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| invalid())
}

pub(super) struct Writer {
    pub dir: Directory,
    blobs: Directory,
    _lock: File,
    journal: Option<File>,
    pub meta: Meta,
    pub checkpoint: Checkpoint,
    bytes: u64,
}
impl Writer {
    pub fn create(root: &Directory, meta: Meta, checkpoint: Checkpoint) -> io::Result<Self> {
        let runs = root.dir("runs", true)?;
        let dir = runs.dir(&meta.run_epoch.to_string(), true)?;
        let lock = match dir.open("lock", true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                dir.open("lock", false)?
            }
            Err(error) => return Err(error),
        };
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let blobs = dir.dir("blobs", true)?;
        let mut result = Self {
            dir,
            blobs,
            _lock: lock,
            journal: None,
            meta,
            checkpoint,
            bytes: 0,
        };
        result.segment()?;
        result.commit(false)?;
        Ok(result)
    }
    fn segment(&mut self) -> io::Result<()> {
        if self.meta.segments.len() >= 4096 {
            return Err(invalid());
        }
        let base = self.blobs.blob(&encode(&self.checkpoint)?)?;
        let id = Uuid::new_v4();
        let file = self.dir.open(&format!("observations.{id}.jsonl"), true)?;
        self.dir.sync()?;
        self.meta.segments.push(Segment {
            id,
            base,
            bytes: 0,
            record_seq: self.checkpoint.record_seq,
            view_seq: self.checkpoint.sequence(),
        });
        self.bytes = 0;
        self.journal = Some(file);
        Ok(())
    }
    pub fn rebase(&mut self, mut checkpoint: Checkpoint, reason: &str) -> io::Result<()> {
        checkpoint.earlier_before =
            (self.meta.saved_through_view_seq > 0).then_some(self.meta.saved_through_view_seq);
        self.meta.gaps.push(Gap {
            after_view_seq: self.meta.saved_through_view_seq,
            through_view_seq: checkpoint.sequence(),
            after_record_seq: self.meta.saved_record_seq,
            through_record_seq: checkpoint.record_seq,
            reason: reason.into(),
        });
        self.checkpoint = checkpoint;
        self.segment()?;
        self.commit(false)
    }
    pub fn append(
        &mut self,
        record_seq: u64,
        view_seq: u64,
        kind: &str,
        data: Value,
    ) -> io::Result<()> {
        if record_seq <= self.checkpoint.record_seq {
            return Ok(());
        }
        if record_seq != self.checkpoint.record_seq + 1 {
            return Err(invalid());
        }
        match kind {
            "view" => self.checkpoint.event(&data).map_err(|_| invalid())?,
            "view_reset" => self.checkpoint = reset(&data, self.meta.run_epoch, &self.checkpoint)?,
            "document" => self
                .checkpoint
                .document(serde_json::from_value::<Document>(data.clone()).map_err(|_| invalid())?),
            _ => return Err(invalid()),
        }
        if self.checkpoint.sequence() != view_seq {
            return Err(invalid());
        }
        // Large safe context documents are written and synced before a journal
        // record can refer to them. No raw request/header type enters this API.
        let encoded = encode(&data)?;
        let data = if encoded.len() > 64 * 1024 {
            json!({"blob":self.blobs.blob(&encoded)?})
        } else {
            data
        };
        let record = Record {
            format_version: FORMAT,
            run_epoch: self.meta.run_epoch,
            record_seq,
            view_seq,
            kind: kind.into(),
            data,
        };
        let digest = blake3::hash(&encode(&record)?).to_hex().to_string();
        let mut bytes = encode(&Line { record, digest })?;
        if bytes.len() >= LINE_LIMIT {
            return Err(invalid());
        }
        bytes.push(b'\n');
        self.journal
            .as_mut()
            .ok_or_else(invalid)?
            .write_all(&bytes)?;
        self.bytes += bytes.len() as u64;
        self.checkpoint.record_seq = record_seq;
        Ok(())
    }
    pub fn commit(&mut self, ended: bool) -> io::Result<()> {
        self.commit_when(|| ended)
    }
    pub fn commit_when(&mut self, mut ended: impl FnMut() -> bool) -> io::Result<()> {
        self.journal.as_mut().ok_or_else(invalid)?.sync_all()?;
        let mut next = self.meta.clone();
        let segment = next.segments.last_mut().ok_or_else(invalid)?;
        segment.bytes = self.bytes;
        segment.record_seq = self.checkpoint.record_seq;
        segment.view_seq = self.checkpoint.sequence();
        next.saved_through_view_seq = self.checkpoint.sequence();
        next.saved_record_seq = self.checkpoint.record_seq;
        if next.gaps.is_empty() {
            next.persisted_through_view_seq = next.saved_through_view_seq;
        }
        // Recheck after the potentially slow journal fsync. A shutdown deadline
        // may have expired since this commit started.
        next.ended = ended();
        // The commit marker is replaced only after all referenced data is
        // synced. An OS-cache write or queue acceptance never advances status.
        let committed = encode(&next)?;
        self.dir.atomic("meta.json", &committed)?;
        self.meta = next;
        if self.meta.ended {
            // Rebuildable acceleration artifact; journal/segment bases remain
            // authoritative if a crash interrupts this optional replacement.
            let _ = self.dir.atomic("snapshot.json", &encode(&json!({"formatVersion":FORMAT,"runEpoch":self.meta.run_epoch,"state":self.checkpoint}))?);
            let _ = super::lifecycle::record(&self.dir, &self.meta, &committed, ended);
        }
        Ok(())
    }
}

pub(super) fn read_meta(dir: &Directory, epoch: Uuid) -> io::Result<Meta> {
    let meta: Meta =
        serde_json::from_slice(&dir.read("meta.json", 4 * 1024 * 1024)?).map_err(|_| invalid())?;
    if meta.format_version != FORMAT
        || meta.run_epoch != epoch
        || meta.segments.is_empty()
        || meta.segments.len() > 4096
        || meta.persisted_through_view_seq > meta.saved_through_view_seq
        || meta.workspace_id.len() != 64
    {
        return Err(invalid());
    }
    Ok(meta)
}
pub(super) fn active(dir: &Directory) -> io::Result<bool> {
    let lock = dir.open("lock", false)?;
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(true)
    } else {
        Err(error)
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub segment: Uuid,
    pub byte_offset: u64,
    pub code: &'static str,
}
pub(super) struct Recovered {
    pub checkpoint: Checkpoint,
    pub issues: Vec<Issue>,
    pub verified_prefix: u64,
}

pub(super) fn recover(dir: &Directory, meta: &Meta, before: Option<u64>) -> io::Result<Recovered> {
    recover_bounded(dir, meta, before, None)
}
pub(super) fn recover_bounded(
    dir: &Directory,
    meta: &Meta,
    before: Option<u64>,
    deadline: Option<std::time::Instant>,
) -> io::Result<Recovered> {
    let blobs = dir.dir("blobs", false)?;
    let mut read_bytes = 0u64;
    let mut checkpoint = None;
    let mut issues = Vec::new();
    let mut verified_prefix = 0;
    let mut intact = true;
    for segment in &meta.segments {
        if deadline.is_some_and(|d| std::time::Instant::now() > d) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let budget = if deadline.is_some() {
            (64 * 1024 * 1024u64).saturating_sub(read_bytes) as usize
        } else {
            FILE_LIMIT
        };
        if deadline.is_some()
            && blobs
                .entry_info(&segment.base)
                .is_ok_and(|i| i.bytes > budget as u64)
        {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let initial: Checkpoint = match blobs
            .read_blob_limited(&segment.base, budget)
            .and_then(|b| serde_json::from_slice::<Checkpoint>(&b).map_err(|_| invalid()))
        {
            Ok(value) if value.validate(meta.run_epoch) => value,
            _ => {
                intact = false;
                issues.push(Issue {
                    segment: segment.id,
                    byte_offset: 0,
                    code: "snapshot_invalid",
                });
                continue;
            }
        };
        if before.is_some_and(|limit| initial.sequence() > limit) {
            break;
        }
        if deadline.is_some() {
            read_bytes += encode(&initial)?.len() as u64;
        }
        let mut state = initial;
        let file = match dir.open(&format!("observations.{}.jsonl", segment.id), false) {
            Ok(file) => file,
            Err(_) => {
                intact = false;
                issues.push(Issue {
                    segment: segment.id,
                    byte_offset: 0,
                    code: "journal_unavailable",
                });
                checkpoint = Some(state);
                continue;
            }
        };
        let len = file.metadata()?.len();
        if len != segment.bytes {
            issues.push(Issue {
                segment: segment.id,
                byte_offset: len.min(segment.bytes),
                code: if len < segment.bytes {
                    "committed_tail_missing"
                } else {
                    "unsaved_tail"
                },
            });
        }
        let mut reader = BufReader::new(file.take(segment.bytes));
        let mut offset = 0;
        let mut valid_prefix = true;
        let mut cutoff = false;
        loop {
            if deadline
                .is_some_and(|d| std::time::Instant::now() > d || read_bytes > 64 * 1024 * 1024)
            {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut bytes = Vec::new();
            let count = reader
                .by_ref()
                .take(LINE_LIMIT as u64 + 1)
                .read_until(b'\n', &mut bytes)?;
            if count == 0 {
                break;
            }
            read_bytes += count as u64;
            let location = offset;
            offset += count as u64;
            let parsed = (|| -> io::Result<()> {
                if bytes.len() > LINE_LIMIT || bytes.last() != Some(&b'\n') {
                    return Err(invalid());
                }
                let line: Line = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                let record = line.record;
                if before.is_some_and(|limit| record.view_seq > limit) {
                    cutoff = true;
                    return Ok(());
                }
                if record.format_version != FORMAT
                    || record.run_epoch != meta.run_epoch
                    || record.record_seq != state.record_seq + 1
                    || blake3::hash(&encode(&record)?).to_hex().as_str() != line.digest
                {
                    return Err(invalid());
                }
                let data = if let Some(id) = record.data.get("blob").and_then(Value::as_str) {
                    let remaining = if deadline.is_some() {
                        (64 * 1024 * 1024u64).saturating_sub(read_bytes) as usize
                    } else {
                        FILE_LIMIT
                    };
                    let blob = blobs.read_blob_limited(id, remaining)?;
                    read_bytes += blob.len() as u64;
                    serde_json::from_slice(&blob).map_err(|_| invalid())?
                } else {
                    record.data
                };
                let expected = state.sequence() + u64::from(record.kind != "document");
                if record.view_seq != expected {
                    return Err(invalid());
                }
                match record.kind.as_str() {
                    "view" => state.event(&data).map_err(|_| invalid())?,
                    "view_reset" => state = reset(&data, meta.run_epoch, &state)?,
                    "document" => {
                        state.document(serde_json::from_value(data).map_err(|_| invalid())?)
                    }
                    _ => return Err(invalid()),
                }
                if state.sequence() != record.view_seq {
                    return Err(invalid());
                }
                state.record_seq = record.record_seq;
                Ok(())
            })();
            if cutoff {
                break;
            }
            if parsed.is_err() {
                issues.push(Issue {
                    segment: segment.id,
                    byte_offset: location,
                    code: if bytes.last() != Some(&b'\n') {
                        "partial_line"
                    } else {
                        "invalid_record"
                    },
                });
                valid_prefix = false;
                // A bad delta cannot be skipped while claiming an intact
                // prefix. The next independent segment can still be restored.
                break;
            }
        }
        if valid_prefix
            && !cutoff
            && (offset != segment.bytes
                || state.record_seq != segment.record_seq
                || state.sequence() != segment.view_seq)
        {
            valid_prefix = false;
            issues.push(Issue {
                segment: segment.id,
                byte_offset: offset,
                code: "checkpoint_mismatch",
            });
        }
        if intact {
            verified_prefix = state.sequence();
        }
        if !valid_prefix {
            intact = false;
        }
        checkpoint = Some(state);
    }
    let checkpoint = checkpoint.ok_or_else(invalid)?;
    if encode(&checkpoint)?.len() > FILE_LIMIT {
        return Err(invalid());
    }
    Ok(Recovered {
        checkpoint,
        issues,
        verified_prefix,
    })
}

fn reset(data: &Value, epoch: Uuid, previous: &Checkpoint) -> io::Result<Checkpoint> {
    let mut checkpoint: Checkpoint =
        serde_json::from_value(data["checkpoint"].clone()).map_err(|_| invalid())?;
    if !checkpoint.validate(epoch)
        || checkpoint.sequence() != previous.sequence() + 1
        || data["event"]["viewSeq"] != checkpoint.sequence()
        || data["event"]["runEpoch"] != epoch.to_string()
    {
        return Err(invalid());
    }
    let removed = previous.snapshot["items"].as_array().is_some_and(|items| {
        items.iter().any(|old| {
            !checkpoint.snapshot["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|new| new["itemKey"] == old["itemKey"])
        })
    });
    checkpoint.earlier_before = if removed && previous.sequence() > 0 {
        Some(previous.sequence())
    } else {
        previous.earlier_before
    };
    Ok(checkpoint)
}
