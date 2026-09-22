//! The browser pairing capability lives only in a private per-run entry file.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEntry {
    pub format: String,
    pub run_epoch: Uuid,
    pub address: String,
    pub url: String,
    pub cli_pid: u32,
}
pub(super) struct EntryFile {
    pub path: PathBuf,
    identity: (u64, u64),
    directory: Option<PathBuf>,
}
impl EntryFile {
    pub fn create(path: Option<&Path>, entry: &BrowserEntry) -> Result<Self> {
        let directory = if path.is_none() {
            let directory = std::env::temp_dir().join(format!("codex-view-{}", entry.run_epoch));
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
            Some(directory)
        } else {
            None
        };
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| directory.as_ref().unwrap().join("entry.json"));
        let write = (|| -> Result<Self> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            let meta = file.metadata()?;
            let owner = Self {
                path: path.clone(),
                identity: (meta.dev(), meta.ino()),
                directory: directory.clone(),
            };
            serde_json::to_writer(&mut file, entry)?;
            file.flush()?;
            Ok(owner)
        })();
        if write.is_err()
            && let Some(directory) = &directory
        {
            let _ = std::fs::remove_dir(directory);
        }
        write
    }
}
impl Drop for EntryFile {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
        if let Some(directory) = &self.directory {
            let _ = std::fs::remove_dir(directory);
        }
    }
}
pub fn read_entry(path: &Path) -> Result<BrowserEntry> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.mode() & 0o077 == 0 && meta.uid() == unsafe { libc::geteuid() },
        "pairing entry must be an owner-only regular file"
    );
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024, "pairing entry is too large");
    let entry: BrowserEntry = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid pairing entry; details suppressed"))?;
    let url = reqwest::Url::parse(&entry.url)
        .map_err(|_| anyhow::anyhow!("invalid pairing entry URL"))?;
    ensure!(
        entry.format == "codex-view-entry-v1"
            && !entry.run_epoch.is_nil()
            && url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none(),
        "invalid local pairing entry"
    );
    ensure!(
        entry.address == url.origin().ascii_serialization(),
        "pairing origin mismatch"
    );
    let pair = url
        .fragment()
        .and_then(|s| s.strip_prefix("pair="))
        .ok_or_else(|| anyhow::anyhow!("missing pairing capability"))?;
    ensure!(
        pair.len() == 64 && pair.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid pairing capability"
    );
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn private_entry_is_owned_readable_and_removed_only_if_still_owned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entry.json");
        let entry = BrowserEntry {
            format: "codex-view-entry-v1".into(),
            run_epoch: Uuid::new_v4(),
            address: "http://127.0.0.1:12345".into(),
            url: format!("http://127.0.0.1:12345/#pair={}", "a".repeat(64)),
            cli_pid: 123,
        };
        let owned = EntryFile::create(Some(&path), &entry).unwrap();
        assert_eq!(read_entry(&path).unwrap().run_epoch, entry.run_epoch);
        assert!(EntryFile::create(Some(&path), &entry).is_err());
        drop(owned);
        assert!(!path.exists());
        let owned = EntryFile::create(Some(&path), &entry).unwrap();
        std::fs::rename(&path, dir.path().join("original")).unwrap();
        std::fs::write(&path, "user replacement").unwrap();
        drop(owned);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user replacement");
    }

    #[test]
    fn entry_reader_rejects_nonlocal_capabilities_and_unsafe_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entry.json");
        let mut entry = BrowserEntry {
            format: "codex-view-entry-v1".into(),
            run_epoch: Uuid::new_v4(),
            address: "http://127.0.0.1:12345".into(),
            url: format!("http://127.0.0.1:12345/#pair={}", "a".repeat(64)),
            cli_pid: 123,
        };
        let good = entry.url.clone();
        for url in [
            "https://127.0.0.1:12345/".to_owned(),
            "http://example.invalid:12345/".to_owned(),
            "http://localhost:12345/".to_owned(),
            good.replace("12345/", "12346/"),
            good.replace("127.0.0.1", "user@127.0.0.1"),
            good.replace("/#", "/other#"),
            good.replace("/#", "/?query=yes#"),
            good.replace(&"a".repeat(64), "invalid-private-capability"),
        ] {
            entry.url = url;
            let file = EntryFile::create(Some(&path), &entry).unwrap();
            let error = read_entry(&path).err().unwrap().to_string();
            assert!(!error.contains(&entry.url));
            drop(file);
        }
        entry.url = good;
        let owned = EntryFile::create(Some(&path), &entry).unwrap();
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(read_entry(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_entry(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, vec![b' '; 16 * 1024 + 1]).unwrap();
        assert!(read_entry(&path).is_err());
        drop(owned);
        assert!(read_entry(dir.path()).is_err());
    }
}
