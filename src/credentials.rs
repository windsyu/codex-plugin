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
