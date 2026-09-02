use std::fs::{self, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

pub fn set_private_umask() {
    #[cfg(unix)]
    // SAFETY: this runs before worker threads are started and intentionally
    // keeps the process-wide mask private for the daemon lifetime.
    unsafe {
        libc::umask(0o077);
    }
}

pub fn prepare_private_dir(path: &Path, label: &str) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("create {label} directory {}", path.display()))?;
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("{label} directory must be a direct directory");
    }
    #[cfg(unix)]
    {
        if metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("{label} directory must be owned by the current user");
        }
        if metadata.mode() & 0o777 != 0o700 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("set {label} directory mode 0700"))?;
        }
    }
    Ok(())
}

pub fn prepare_private_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("{label} must be a direct regular file");
    }
    #[cfg(unix)]
    {
        if metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("{label} must be owned by the current user");
        }
        if metadata.mode() & 0o777 != 0o600 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("set {label} mode 0600"))?;
        }
        if let Some(parent) = path.parent() {
            prepare_private_dir(parent, label)?;
        }
    }
    Ok(())
}

pub fn create_private_file(path: &Path, label: &str) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        prepare_private_dir(parent, label)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("create {label} {}", path.display()))
}

#[cfg(unix)]
pub fn validate_private_unix_socket(path: &Path, label: &str) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        anyhow::bail!("{label} is not a direct Unix socket");
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        anyhow::bail!("{label} is not owned by the current user");
    }
    if metadata.mode() & 0o077 != 0 {
        anyhow::bail!("{label} permissions are broader than 0600");
    }
    let parent = path.parent().context("Unix socket has no parent")?;
    let parent_metadata = fs::metadata(parent)?;
    if parent_metadata.uid() != unsafe { libc::geteuid() } || parent_metadata.mode() & 0o022 != 0 {
        anyhow::bail!("{label} directory is not private");
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn validate_private_unix_socket(_path: &Path, _label: &str) -> Result<()> {
    anyhow::bail!("App Server live mode is currently supported on Unix only")
}

pub fn prepare_database_files(path: &Path) -> Result<()> {
    let suffix = |value: &str| {
        let mut name = path.as_os_str().to_os_string();
        name.push(value);
        std::path::PathBuf::from(name)
    };
    for (candidate, label) in [
        (path.to_path_buf(), "Observer database"),
        (suffix("-wal"), "Observer WAL"),
        (suffix("-shm"), "Observer SHM"),
    ] {
        if fs::symlink_metadata(&candidate).is_ok() {
            prepare_private_file(&candidate, label)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn repairs_owned_private_modes_and_rejects_symlinks() -> Result<()> {
        let temp = TempDir::new()?;
        let dir = temp.path().join("observer-data");
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
        prepare_private_dir(&dir, "test")?;
        assert_eq!(fs::metadata(&dir)?.mode() & 0o777, 0o700);

        let file = dir.join("observer.sqlite");
        fs::write(&file, b"")?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        prepare_private_file(&file, "test")?;
        assert_eq!(fs::metadata(&file)?.mode() & 0o777, 0o600);

        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link)?;
        assert!(prepare_private_file(&link, "test").is_err());
        Ok(())
    }
}
