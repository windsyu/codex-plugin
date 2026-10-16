//! Private connector state. Never included in ordinary application configuration.
use anyhow::{Context, Result, bail};
use codex_relay_protocol::Bound;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Saved {
    pub server_url: String,
    pub name: String,
    pub allow_insecure_loopback: bool,
    pub enabled: bool,
    #[serde(default)]
    pub authentication_required: bool,
    pub bound: Option<Bound>,
}

pub(super) struct Storage {
    dir: PathBuf,
}
impl Storage {
    pub fn open(root: &Path) -> Result<Self> {
        check(root, true, false)?;
        let dir = root.join("relay");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        #[cfg(not(unix))]
        if !dir.exists() {
            fs::create_dir(&dir)?;
        }
        check(&dir, true, true)?;
        Ok(Self { dir })
    }
    pub fn load(&self) -> Result<Saved> {
        check(&self.dir, true, true)?;
        let path = self.dir.join("state.json");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
            Err(e) => return Err(e.into()),
            Ok(_) => check(&path, false, true)?,
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path).context("open private relay state")?;
        if file.metadata()?.len() > 32 * 1024 {
            bail!("private relay state exceeds limit");
        }
        let mut bytes = Vec::new();
        file.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid private relay state"))
    }
    pub fn save(&self, saved: &Saved) -> Result<()> {
        check(&self.dir, true, true)?;
        let dest = self.dir.join("state.json");
        if fs::symlink_metadata(&dest).is_ok() {
            check(&dest, false, true)?;
        }
        let temp = self.dir.join(format!(".{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec(saved)?)?;
            file.sync_all()?;
            fs::rename(&temp, &dest)?;
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}
fn check(path: &Path, directory: bool, private: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path).context("inspect relay state location")?;
    if meta.file_type().is_symlink()
        || (directory && !meta.is_dir())
        || (!directory && !meta.is_file())
    {
        bail!("unsafe relay state location");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() } {
            bail!("relay state has another owner");
        }
        if private && meta.permissions().mode() & 0o077 != 0 {
            bail!("relay state must be private to its owner");
        }
        if directory && meta.permissions().mode() & 0o022 != 0 {
            bail!("relay state parent must not be writable by another user");
        }
        if !directory && meta.nlink() != 1 {
            bail!("relay state must not be hard linked");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_roundtrip_private_atomic_and_forget() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path()).unwrap();
        assert!(!store.load().unwrap().enabled);
        let mut state = Saved {
            server_url: "https://relay.example".into(),
            enabled: true,
            bound: Some(Bound {
                username: "synthetic".into(),
                account_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                credential: "synthetic-private-credential".into(),
                gateway_origin: "https://gateway.example".into(),
            }),
            ..Saved::default()
        };
        store.save(&state).unwrap();
        assert!(store.load().unwrap().enabled);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(root.path().join("relay/state.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        state.bound = None;
        store.save(&state).unwrap();
        assert!(
            !fs::read_to_string(root.path().join("relay/state.json"))
                .unwrap()
                .contains("synthetic-private-credential")
        );
        assert_eq!(fs::read_dir(root.path().join("relay")).unwrap().count(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_and_nonprivate_storage() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        symlink(other.path(), root.path().join("relay")).unwrap();
        assert!(Storage::open(root.path()).is_err());
        fs::remove_file(root.path().join("relay")).unwrap();
        let store = Storage::open(root.path()).unwrap();
        symlink(
            other.path().join("secret"),
            root.path().join("relay/state.json"),
        )
        .unwrap();
        assert!(store.save(&Saved::default()).is_err());
        assert!(store.load().is_err());
        fs::remove_file(root.path().join("relay/state.json")).unwrap();
        store.save(&Saved::default()).unwrap();
        fs::set_permissions(
            root.path().join("relay/state.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(store.load().is_err());
    }
}
