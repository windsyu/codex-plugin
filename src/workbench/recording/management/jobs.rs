use super::super::fs::{Directory, Entries, Identity};
use super::{
    cleanup::{self, Job},
    model::Error,
};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use uuid::Uuid;

pub(super) fn root(path: &Path, identity: Option<Identity>) -> Result<Directory, Error> {
    let identity = identity.ok_or(Error::Busy)?;
    let root = Directory::root(path).map_err(|_| Error::Unavailable)?;
    if root.identity().map_err(|_| Error::Unavailable)? != identity {
        return Err(Error::Unavailable);
    }
    Ok(root)
}
pub(super) struct Cursor {
    pub root: Directory,
    directory: Directory,
    entries: Entries,
    prefix: Uuid,
    offset: usize,
    revision: String,
    previous: Option<(String, Value)>,
}
impl Cursor {
    pub fn new(root: Directory) -> io::Result<Self> {
        let directory = root.dir("cleanup", false)?.dir("jobs", false)?;
        let info = root.dir("cleanup", false)?.entry_info("jobs")?;
        let revision = format!(
            "{}:{}:{}",
            info.identity.inode, info.modified.0, info.modified.1
        );
        let entries = directory.entries()?;
        Ok(Self {
            root,
            directory,
            entries,
            prefix: Uuid::new_v4(),
            offset: 0,
            revision,
            previous: None,
        })
    }
    pub fn page(&mut self, workspace: &str, cursor: Option<&str>) -> Result<Value, Error> {
        self.verify_revision()?;
        let key = cursor.unwrap_or("");
        if let Some((old, value)) = &self.previous
            && old == key
        {
            return Ok(value.clone());
        }
        if cursor.is_some_and(|c| c != format!("{}.{}", self.prefix, self.offset)) {
            return Err(Error::Stale);
        }
        let mut rows = vec![];
        let mut more = true;
        let mut unavailable = 0;
        for _ in 0..128 {
            let Some(entry) = self.entries.next() else {
                more = false;
                break;
            };
            self.offset += 1;
            let Some(id) = entry.ok().and_then(|s| {
                s.strip_suffix(".json")
                    .and_then(|s| Uuid::parse_str(s).ok())
            }) else {
                unavailable += 1;
                continue;
            };
            if let Ok(job) = cleanup::read_job(&self.root, workspace, id) {
                rows.push(job.value());
            }
            if rows.len() == 20 {
                break;
            }
        }
        // Another launcher may publish a job while this page is being read.
        self.verify_revision()?;
        let value = json!({"jobs":rows,"nextCursor":more.then(||format!("{}.{}",self.prefix,self.offset)),"unverifiedEntries":unavailable});
        self.previous = Some((key.into(), value.clone()));
        Ok(value)
    }
    fn verify_revision(&self) -> Result<(), Error> {
        self.directory
            .verify_location()
            .map_err(|_| Error::Unavailable)?;
        let info = self
            .root
            .dir("cleanup", false)
            .and_then(|d| d.entry_info("jobs"))
            .map_err(|_| Error::Unavailable)?;
        if self.revision
            != format!(
                "{}:{}:{}",
                info.identity.inode, info.modified.0, info.modified.1
            )
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn next_pending(&mut self, workspace: &str) -> io::Result<(bool, Option<Job>)> {
        self.root.verify_location()?;
        self.directory.verify_location()?;
        for _ in 0..128 {
            let Some(entry) = self.entries.next() else {
                return Ok((true, None));
            };
            let Some(id) = entry.ok().and_then(|s| {
                s.strip_suffix(".json")
                    .and_then(|s| Uuid::parse_str(s).ok())
            }) else {
                continue;
            };
            if let Ok(job) = cleanup::read_job(&self.root, workspace, id)
                && job.pending()
            {
                return Ok((false, Some(job)));
            }
        }
        Ok((false, None))
    }
}
