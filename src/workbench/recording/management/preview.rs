use super::super::fs::{Directory, Entries, Identity};
use super::model::*;
use super::scan::{self, Measure};
use std::collections::VecDeque;
use std::fs::File;
use std::io;
use uuid::Uuid;

pub(super) struct Builder {
    pub id: Uuid,
    root: Directory,
    runs: Directory,
    _lock: Option<File>,
    ids: VecDeque<Uuid>,
    entries: Option<Entries>,
    days: Option<u32>,
    measure: Option<(Uuid, Measure)>,
}
impl Builder {
    pub fn begin(path: &std::path::Path, mode: PreviewMode, scan_locked: bool) -> io::Result<Self> {
        let root = Directory::root(path)?;
        let runs = root.dir("runs", false)?;
        let held = if scan_locked {
            None
        } else {
            Some(scan::lock(&root.dir("usage-v1", true)?, "lock", true)?)
        };
        let (ids, entries, days) = match mode {
            PreviewMode::Manual(ids) => (ids.into(), None, None),
            PreviewMode::Retention(days) => (VecDeque::new(), Some(runs.entries()?), Some(days)),
        };
        Ok(Self {
            id: Uuid::nil(),
            root,
            runs,
            _lock: held,
            ids,
            entries,
            days,
            measure: None,
        })
    }
    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }
    pub fn identity(&self) -> io::Result<Identity> {
        self.root.identity()
    }
    pub fn step(&mut self, workspace: &str, current: Uuid, p: &mut Preview) -> io::Result<bool> {
        self.root.verify_location()?;
        self.runs.verify_location()?;
        if let Some((id, m)) = &mut self.measure {
            let id = *id;
            match m.step(128) {
                Ok(false) => return Ok(false),
                Ok(true) => {
                    let (_, m) = self.measure.take().unwrap();
                    let c = m.finish();
                    if let Some(days) = self.days {
                        if !scan::expired(&c.row, days, chrono::Utc::now()) {
                            let code = c
                                .row
                                .reason
                                .clone()
                                .or(c.row.retention_reason.clone())
                                .unwrap_or_else(|| "not_expired".into());
                            *p.skipped_counts.entry(code).or_default() += 1;
                        } else {
                            p.items.push(PreviewItem {
                                run_epoch: id,
                                eligible: true,
                                reason: None,
                                run: Some(c.row.clone()),
                            });
                            p.candidates.push(c);
                        }
                    } else {
                        p.items.push(PreviewItem {
                            run_epoch: id,
                            eligible: c.row.manual_eligible,
                            reason: c.row.reason.clone(),
                            run: Some(c.row.clone()),
                        });
                        if c.row.manual_eligible {
                            p.candidates.push(c);
                        }
                    }
                }
                Err(_) => {
                    self.measure = None;
                    self.skipped(p, id, "identity_changed_or_unreadable");
                }
            }
        }
        if p.items.len() >= 100 {
            p.scan_complete = self.days.is_none() && self.ids.is_empty();
            return Ok(true);
        }
        for _ in 0..128 {
            let id = if let Some(entries) = &mut self.entries {
                let Some(next) = entries.next() else {
                    p.scan_complete = true;
                    return Ok(true);
                };
                let Some(id) = next
                    .ok()
                    .and_then(|s| Uuid::parse_str(&s).ok().filter(|id| id.to_string() == s))
                else {
                    *p.skipped_counts
                        .entry("unverified_entry".into())
                        .or_default() += 1;
                    continue;
                };
                id
            } else {
                let Some(id) = self.ids.pop_front() else {
                    p.scan_complete = true;
                    return Ok(true);
                };
                id
            };
            match Measure::begin(&self.runs, id, workspace, current) {
                Ok(Some(m)) => {
                    self.measure = Some((id, m));
                    return Ok(false);
                }
                Ok(None) => {
                    if self.days.is_none() {
                        self.skipped(p, id, "not_found");
                    }
                }
                Err(_) => self.skipped(p, id, "not_found_or_unverifiable"),
            }
        }
        Ok(false)
    }
    fn skipped(&self, p: &mut Preview, id: Uuid, code: &str) {
        if self.days.is_some() {
            *p.skipped_counts.entry(code.into()).or_default() += 1;
        } else {
            p.items.push(PreviewItem {
                run_epoch: id,
                eligible: false,
                reason: Some(code.into()),
                run: None,
            });
        }
    }
}
