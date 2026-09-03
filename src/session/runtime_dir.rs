use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::permissions::{create_private_file, prepare_private_dir, prepare_private_file};

const MARKER: &str = "codex-local-observer-session-v1\n";
const MARKER_NAME: &str = ".session-owner";

#[derive(Debug, Clone)]
pub struct RuntimeRoot {
    root: PathBuf,
}

impl RuntimeRoot {
    pub fn prepare(root: PathBuf) -> Result<Self> {
        prepare_private_dir(&root, "Session Kernel runtime")?;
        let runtime = Self { root };
        runtime.sweep_orphans()?;
        Ok(runtime)
    }

    pub fn create_worker_dir(&self, worker_id: &str) -> Result<PathBuf> {
        validate_worker_id(worker_id)?;
        let path = self.root.join(format!("worker-{worker_id}"));
        fs::create_dir(&path).with_context(|| {
            format!("create Session Worker runtime directory {}", path.display())
        })?;
        prepare_private_dir(&path, "Session Worker runtime")?;
        let mut marker = create_private_file(&path.join(MARKER_NAME), "Session Worker marker")?;
        use std::io::Write as _;
        marker.write_all(MARKER.as_bytes())?;
        marker.sync_all()?;
        Ok(path)
    }

    pub fn cleanup_worker_dir(&self, path: &Path) -> Result<()> {
        self.verify_managed_worker_dir(path)?;
        fs::remove_dir_all(path)
            .with_context(|| format!("remove Session Worker runtime directory {}", path.display()))
    }

    pub fn sweep_orphans(&self) -> Result<usize> {
        let mut swept = 0;
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("scan Session Kernel runtime {}", self.root.display()))?
        {
            let path = entry?.path();
            if self.verify_managed_worker_dir(&path).is_ok() {
                fs::remove_dir_all(&path).with_context(|| {
                    format!("remove orphan Session Worker runtime {}", path.display())
                })?;
                swept += 1;
            }
        }
        Ok(swept)
    }

    fn verify_managed_worker_dir(&self, path: &Path) -> Result<()> {
        if path.parent() != Some(self.root.as_path()) {
            anyhow::bail!("Session Worker runtime is outside the configured root");
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Session Worker runtime has no UTF-8 name")?;
        let worker_id = name
            .strip_prefix("worker-")
            .context("Session Worker runtime has an unmanaged name")?;
        validate_worker_id(worker_id)?;
        prepare_private_dir(path, "Session Worker runtime")?;
        let marker = path.join(MARKER_NAME);
        prepare_private_file(&marker, "Session Worker marker")?;
        if fs::read_to_string(&marker)? != MARKER {
            anyhow::bail!("Session Worker marker is invalid");
        }
        Ok(())
    }
}

fn validate_worker_id(worker_id: &str) -> Result<()> {
    let parsed = Uuid::parse_str(worker_id).context("Session Worker id is invalid")?;
    if parsed.to_string() != worker_id {
        anyhow::bail!("Session Worker id is not canonical");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn creates_private_worker_dirs_and_sweeps_only_marked_children() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new()?;
        let root = RuntimeRoot::prepare(temp.path().join("runtime"))?;
        let managed = root.create_worker_dir(&Uuid::new_v4().to_string())?;
        assert_eq!(fs::metadata(&managed)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(managed.join(MARKER_NAME))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let unrelated = temp.path().join("runtime/not-a-worker");
        fs::create_dir(&unrelated)?;
        let symlink_target = temp.path().join("must-survive");
        fs::create_dir(&symlink_target)?;
        let symlink_worker = temp
            .path()
            .join(format!("runtime/worker-{}", Uuid::new_v4()));
        symlink(&symlink_target, &symlink_worker)?;
        let invalid_marker = root.create_worker_dir(&Uuid::new_v4().to_string())?;
        fs::write(
            invalid_marker.join(MARKER_NAME),
            b"not-owned-by-session-kernel\n",
        )?;
        assert_eq!(root.sweep_orphans()?, 1);
        assert!(!managed.exists());
        assert!(unrelated.exists());
        assert!(symlink_worker.is_symlink());
        assert!(symlink_target.exists());
        assert!(invalid_marker.exists());
        Ok(())
    }
}
