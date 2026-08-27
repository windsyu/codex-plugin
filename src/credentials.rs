use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;

use crate::permissions::{prepare_private_dir, prepare_private_file};

pub fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    if let Some(parent) = path.parent() {
        prepare_private_dir(parent, "fingerprint key")?;
    }
    if fs::symlink_metadata(path).is_ok() {
        prepare_private_file(path, "fingerprint key")?;
        let bytes =
            fs::read(path).with_context(|| format!("read fingerprint key {}", path.display()))?;
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("fingerprint key must contain exactly 32 bytes"));
    }
    let mut key = [0_u8; 32];
    rand::rng().fill_bytes(&mut key);
    write_private_new(path, &key)?;
    Ok(key)
}

pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        prepare_private_dir(parent, "bearer token")?;
    }
    if fs::symlink_metadata(path).is_ok() {
        prepare_private_file(path, "bearer token")?;
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    write_private_new(path, format!("{token}\n").as_bytes())?;
    Ok(token)
}

pub fn rotate_token(path: &Path) -> Result<String> {
    let parent = path
        .parent()
        .context("bearer token path must have a parent directory")?;
    prepare_private_dir(parent, "bearer token")?;
    if fs::symlink_metadata(path).is_ok() {
        prepare_private_file(path, "bearer token")?;
    }

    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let file_name = path
        .file_name()
        .context("bearer token path must have a file name")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    if let Err(error) = write_private_new(&temporary, format!("{token}\n").as_bytes()) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("stage bearer token {}", path.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("replace bearer token {}", path.display()));
    }
    Ok(token)
}

fn write_private_new(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rotation_replaces_token_while_load_keeps_current_startup_value() -> Result<()> {
        let temp = TempDir::new()?;
        let path = temp.path().join("observer-data/token");
        let initial = load_or_create_token(&path)?;
        assert_eq!(load_or_create_token(&path)?, initial);

        let first_startup = rotate_token(&path)?;
        assert_ne!(first_startup, initial);
        assert_eq!(load_or_create_token(&path)?, first_startup);

        let second_startup = rotate_token(&path)?;
        assert_ne!(second_startup, first_startup);
        assert_eq!(fs::read_to_string(&path)?.trim(), second_startup);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }
}
