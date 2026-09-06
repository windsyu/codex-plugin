use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

#[cfg(unix)]
pub(super) fn read_only_mode(path: &Path) -> Value {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return json!({"status":"missing"});
    };
    use std::os::unix::fs::MetadataExt;
    json!({"status":if metadata.file_type().is_symlink() {"symlink"} else {"present"},
        "mode":format!("{:04o}",metadata.mode() & 0o777),"ownerIsCurrentUser":metadata.uid() == unsafe { libc::geteuid() }})
}

#[cfg(not(unix))]
pub(super) fn read_only_mode(path: &Path) -> Value {
    json!({"status":if fs::symlink_metadata(path).is_ok() {"present"} else {"missing"}})
}

pub(super) fn write_private_new(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("export output must have a parent directory")?;
    if !parent.is_dir() {
        anyhow::bail!("export output directory does not exist");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(path)
            .with_context(|| format!("create export output {}", path.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}
