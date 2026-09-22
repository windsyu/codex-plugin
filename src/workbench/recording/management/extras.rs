//! Bounded metadata-only accounting, including partially removed quarantines.
use super::super::fs::{Directory, Entries};
use super::{cleanup, model::Size};
use std::io;
use std::time::{Duration, Instant};
use uuid::Uuid;
struct Frame {
    dir: Directory,
    entries: Entries,
    scope: u8,
}
struct Tree {
    frames: Vec<Frame>,
    bytes: u64,
    complete: bool,
}
impl Tree {
    fn new(dir: Directory, scope: u8) -> io::Result<Self> {
        let entries = dir.entries()?;
        Ok(Self {
            frames: vec![Frame {
                dir,
                entries,
                scope,
            }],
            bytes: 0,
            complete: true,
        })
    }
    fn step(&mut self) -> io::Result<bool> {
        let deadline = Instant::now() + Duration::from_millis(20);
        for _ in 0..128 {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let Some(frame) = self.frames.last_mut() else {
                return Ok(true);
            };
            frame.dir.verify_location()?;
            let Some(next) = frame.entries.next() else {
                self.frames.pop();
                continue;
            };
            let Ok(name) = next else {
                self.complete = false;
                continue;
            };
            if (frame.scope == 0 && name == "runs") || (frame.scope == 2 && name == "trash") {
                continue;
            }
            let Ok(info) = frame.dir.entry_info(&name) else {
                self.complete = false;
                continue;
            };
            if !info.directory {
                self.bytes = self.bytes.saturating_add(info.bytes);
                continue;
            }
            let next_scope = match (frame.scope, name.as_str()) {
                (0, "usage-v1") => Some(1),
                (0, "cleanup") => Some(2),
                (2, "jobs" | "removed") => Some(3),
                (4, "blobs") => Some(5),
                _ => None,
            };
            if let Some(scope) = next_scope {
                let dir = frame.dir.dir(&name, false)?;
                if dir.identity()?.device != frame.dir.identity()?.device {
                    self.complete = false;
                    continue;
                }
                let entries = dir.entries()?;
                self.frames.push(Frame {
                    dir,
                    entries,
                    scope,
                });
            } else {
                self.complete = false;
            }
        }
        Ok(false)
    }
}
pub(super) struct Extras {
    shared: Tree,
    shared_done: bool,
    trash: Option<Frame>,
    job: Option<(Frame, cleanup::Job)>,
    remaining: Option<Tree>,
    pending_bytes: u64,
    pending_complete: bool,
}
impl Extras {
    pub fn new(root: &Directory) -> io::Result<Self> {
        let shared = Tree::new(Directory::root(&root.path)?, 0)?;
        let dir = root
            .dir("cleanup", false)
            .and_then(|d| d.dir("trash", false));
        let (mut pending_complete, mut trash) = (true, None);
        match dir {
            Ok(dir) => {
                let entries = dir.entries()?;
                trash = Some(Frame {
                    dir,
                    entries,
                    scope: 6,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => pending_complete = false,
        };
        Ok(Self {
            shared,
            shared_done: false,
            trash,
            job: None,
            remaining: None,
            pending_bytes: 0,
            pending_complete,
        })
    }
    pub fn sizes(&self) -> (Size, Size) {
        (
            Size::new(self.shared.bytes, self.shared_done && self.shared.complete),
            Size::new(
                self.pending_bytes,
                self.pending_complete
                    && self.trash.is_none()
                    && self.job.is_none()
                    && self.remaining.is_none(),
            ),
        )
    }
    pub fn step(&mut self, root: &Directory, workspace: &str) -> io::Result<bool> {
        if !self.shared_done {
            self.shared_done = self.shared.step()?;
            return Ok(false);
        }
        if let Some(tree) = &mut self.remaining {
            match tree.step() {
                Ok(false) => return Ok(false),
                Ok(true) => {
                    self.pending_bytes = self.pending_bytes.saturating_add(tree.bytes);
                    self.pending_complete &= tree.complete;
                }
                Err(_) => {
                    self.pending_bytes = self.pending_bytes.saturating_add(tree.bytes);
                    self.pending_complete = false;
                }
            }
            self.remaining = None;
        }
        for _ in 0..128 {
            if let Some((frame, job)) = &mut self.job {
                frame.dir.verify_location()?;
                let Some(entry) = frame.entries.next() else {
                    self.job = None;
                    continue;
                };
                let Some(id) = entry
                    .ok()
                    .and_then(|s| Uuid::parse_str(&s).ok().filter(|id| id.to_string() == s))
                else {
                    self.pending_complete = false;
                    continue;
                };
                let Some(expected) = job
                    .items
                    .iter()
                    .find(|i| i.run_epoch == id)
                    .and_then(|i| i.candidate.as_ref())
                else {
                    self.pending_complete = false;
                    continue;
                };
                match frame.dir.dir(&id.to_string(), false) {
                    Ok(dir) if dir.identity()? == expected.identity => {
                        self.remaining = Some(Tree::new(dir, 4)?);
                        return Ok(false);
                    }
                    _ => self.pending_complete = false,
                }
            } else if let Some(trash) = &mut self.trash {
                trash.dir.verify_location()?;
                let Some(entry) = trash.entries.next() else {
                    self.trash = None;
                    return Ok(true);
                };
                let Some(id) = entry
                    .ok()
                    .and_then(|s| Uuid::parse_str(&s).ok().filter(|id| id.to_string() == s))
                else {
                    self.pending_complete = false;
                    continue;
                };
                let Ok(job) = cleanup::read_job_any(root, id) else {
                    self.pending_complete = false;
                    continue;
                };
                if job.workspace_id != workspace {
                    continue;
                }
                let dir = trash.dir.dir(&id.to_string(), false)?;
                if dir.identity()? != job.trash_identity {
                    self.pending_complete = false;
                    continue;
                }
                let entries = dir.entries()?;
                self.job = Some((
                    Frame {
                        dir,
                        entries,
                        scope: 6,
                    },
                    job,
                ));
            } else {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
