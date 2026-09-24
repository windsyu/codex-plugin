//! Read-only gate for the currently validated, unmanaged custom-provider path.
//! This does not merge official configuration, select credentials or fetch cloud
//! policy. A source outside the verified matrix fails before proxy/CLI startup.

use std::io::Read;
use std::path::Path;

use anyhow::{Result, bail, ensure};
use serde::Serialize;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSources {
    pub system_config: bool,
    pub managed_file: bool,
    pub system_requirements: bool,
    pub managed_config_preference: bool,
    pub managed_requirements_preference: bool,
    pub unverified_auth_store: bool,
    pub possible_cloud_management: bool,
}
impl ConfigSources {
    pub fn require_verified(&self) -> Result<()> {
        ensure!(
            !self.system_config,
            "system configuration requires a verified effective-route adapter"
        );
        ensure!(
            !self.managed_file
                && !self.system_requirements
                && !self.managed_config_preference
                && !self.managed_requirements_preference,
            "managed configuration is not validated for this capture profile; native policy must not be overridden"
        );
        ensure!(
            !self.unverified_auth_store,
            "credential-store configuration is not validated for this capture profile"
        );
        ensure!(
            !self.possible_cloud_management,
            "cloud-managed configuration is not validated for this capture profile"
        );
        Ok(())
    }
}

pub fn inspect(home: &Path, config: &toml::Value) -> Result<ConfigSources> {
    inspect_for_launch(home, config).map(|(sources, _)| sources)
}

pub(crate) fn inspect_for_launch(
    home: &Path,
    config: &toml::Value,
) -> Result<(ConfigSources, AuthSnapshot)> {
    let (managed_config_preference, managed_requirements_preference) = managed_preferences()?;
    inspect_sources_for_launch(
        home,
        config,
        Path::new("/etc/codex"),
        (managed_config_preference, managed_requirements_preference),
    )
}

fn present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("configuration-source metadata could not be checked"),
    }
}
#[cfg(test)]
fn inspect_sources(
    home: &Path,
    config: &toml::Value,
    system: &Path,
    preferences: (bool, bool),
) -> Result<ConfigSources> {
    inspect_sources_for_launch(home, config, system, preferences).map(|(sources, _)| sources)
}
fn inspect_sources_for_launch(
    home: &Path,
    config: &toml::Value,
    system: &Path,
    preferences: (bool, bool),
) -> Result<(ConfigSources, AuthSnapshot)> {
    let mut sources = ConfigSources {
        system_config: present(&system.join("config.toml"))?,
        managed_file: present(&system.join("managed_config.toml"))?,
        system_requirements: present(&system.join("requirements.toml"))?,
        managed_config_preference: preferences.0,
        managed_requirements_preference: preferences.1,
        // File is the installed baseline's default. Keyring/auto may resolve
        // a ChatGPT account even when auth.json contains an API key or is absent.
        unverified_auth_store: config
            .get("cli_auth_credentials_store")
            .is_some_and(|value| value.as_str() != Some("file")),
        ..ConfigSources::default()
    };
    let snapshot = AuthSnapshot::read(home)?;
    if let Some(bytes) = &snapshot.bytes {
        let value = auth_value(bytes)?;
        sources.possible_cloud_management = !value["tokens"].is_null()
            || value
                .get("auth_mode")
                .is_some_and(|mode| !mode.is_null() && mode.as_str() != Some("apikey"));
    }
    Ok((sources, snapshot))
}

// Credential bytes never enter serialized source diagnostics or Debug output.
pub(crate) struct AuthSnapshot {
    bytes: Option<Vec<u8>>,
    api_key: Option<String>,
}
impl AuthSnapshot {
    fn read(home: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(home.join("auth.json"))
        {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => bail!("native authentication mode could not be checked"),
        };
        let Some(file) = file else {
            return Ok(Self {
                bytes: None,
                api_key: None,
            });
        };
        ensure!(
            file.metadata()
                .map_err(|_| anyhow::anyhow!("native authentication mode could not be checked"))?
                .is_file(),
            "native authentication metadata must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("native authentication mode could not be checked"))?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "native authentication metadata exceeds inspection bound"
        );
        let value = auth_value(&bytes)?;
        let api_key = value
            .get("OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        Ok(Self {
            bytes: Some(bytes),
            api_key,
        })
    }
    pub(crate) fn redaction_secrets(&self) -> Vec<String> {
        self.api_key.iter().cloned().collect()
    }
    pub(crate) fn unchanged(&self, home: &Path) -> Result<()> {
        let current = Self::read(home)?;
        ensure!(
            current.bytes == self.bytes,
            "native authentication metadata changed during startup; retry from the current settings"
        );
        Ok(())
    }
}
fn auth_value(bytes: &[u8]) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
        anyhow::anyhow!("native authentication metadata is invalid; details suppressed")
    })?;
    ensure!(
        value.is_object(),
        "native authentication metadata has an unsupported shape"
    );
    Ok(value)
}

#[cfg(target_os = "macos")]
fn managed_preferences() -> Result<(bool, bool)> {
    use std::ffi::{c_char, c_void};
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            bytes: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFPreferencesCopyAppValue(
            key: *const c_void,
            application_id: *const c_void,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }
    struct Owned(*const c_void);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: Owned contains a non-null Create/Copy result and releases it once.
            unsafe {
                CFRelease(self.0);
            }
        }
    }
    fn string(value: &'static std::ffi::CStr) -> Result<Owned> {
        // SAFETY: Static C strings are NUL terminated; UTF-8 is the declared encoding.
        let pointer =
            unsafe { CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), 0x08000100) };
        ensure!(!pointer.is_null(), "cannot inspect managed preferences");
        Ok(Owned(pointer))
    }
    fn has(key: &'static std::ffi::CStr, domain: &Owned) -> Result<bool> {
        let key = string(key)?;
        // SAFETY: Both arguments remain live CFStrings. The copied value is
        // checked only for presence; neither its type nor secret contents escape.
        let value = unsafe { CFPreferencesCopyAppValue(key.0, domain.0) };
        if value.is_null() {
            Ok(false)
        } else {
            drop(Owned(value));
            Ok(true)
        }
    }
    let domain = string(c"com.openai.codex")?;
    Ok((
        has(c"config_toml_base64", &domain)?,
        has(c"requirements_toml_base64", &domain)?,
    ))
}

#[cfg(not(target_os = "macos"))]
fn managed_preferences() -> Result<(bool, bool)> {
    // Do not claim an unmanaged host on an OS without a verified source inventory.
    bail!("this host configuration inventory has not been validated")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_presence_is_rejected_without_reading_or_overriding_policy() {
        let directory = tempfile::tempdir().unwrap();
        let config = toml::Value::Table(toml::Table::new());
        let system = directory.path().join("system");
        std::fs::create_dir(&system).unwrap();
        inspect_sources(directory.path(), &config, &system, (false, false))
            .unwrap()
            .require_verified()
            .unwrap();
        for name in ["config.toml", "managed_config.toml", "requirements.toml"] {
            let path = system.join(name);
            std::fs::write(
                &path,
                "synthetic policy; must not be parsed as routing overrides",
            )
            .unwrap();
            assert!(
                inspect_sources(directory.path(), &config, &system, (false, false))
                    .unwrap()
                    .require_verified()
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "synthetic policy; must not be parsed as routing overrides"
            );
            std::fs::remove_file(path).unwrap();
        }
        for prefs in [(true, false), (false, true)] {
            assert!(
                inspect_sources(directory.path(), &config, &system, prefs)
                    .unwrap()
                    .require_verified()
                    .is_err()
            );
        }
    }
    #[test]
    fn cloud_auth_unknown_store_and_invalid_metadata_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = toml::Value::Table(toml::Table::new());
        let path = directory.path().join("auth.json");
        for body in [
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic"}}"#,
            r#"{"tokens":{"id_token":"synthetic"}}"#,
            r#"{"auth_mode":"future-mode"}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(
                inspect_sources(directory.path(), &config, directory.path(), (false, false))
                    .unwrap()
                    .require_verified()
                    .is_err()
            );
        }
        std::fs::write(
            &path,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-only","tokens":null}"#,
        )
        .unwrap();
        inspect_sources(directory.path(), &config, directory.path(), (false, false))
            .unwrap()
            .require_verified()
            .unwrap();
        for mode in ["keyring", "auto", "ephemeral", "unknown"] {
            config.as_table_mut().unwrap().insert(
                "cli_auth_credentials_store".into(),
                toml::Value::String(mode.into()),
            );
            assert!(
                inspect_sources(directory.path(), &config, directory.path(), (false, false))
                    .unwrap()
                    .require_verified()
                    .is_err()
            );
        }
        std::fs::write(&path, "{synthetic malformed credential field").unwrap();
        let error = inspect_sources(directory.path(), &config, directory.path(), (false, false))
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("synthetic malformed"));
    }

    #[test]
    fn authentication_snapshot_secrets_stay_out_of_source_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let config = toml::Value::Table(toml::Table::new());
        let path = directory.path().join("auth.json");
        std::fs::write(&path, r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-private-api-key","unrelated":"synthetic-other-value"}"#).unwrap();
        let (sources, snapshot) =
            inspect_sources_for_launch(directory.path(), &config, directory.path(), (false, false))
                .unwrap();
        sources.require_verified().unwrap();
        assert_eq!(
            snapshot.redaction_secrets(),
            vec!["synthetic-private-api-key"]
        );
        let diagnostic = serde_json::to_string(&sources).unwrap();
        assert!(!diagnostic.contains("synthetic"));
        assert!(!diagnostic.contains("OPENAI_API_KEY"));
        snapshot.unchanged(directory.path()).unwrap();
        for body in [
            r#"{}"#,
            r#"{"OPENAI_API_KEY":""}"#,
            r#"{"OPENAI_API_KEY":null}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(
                AuthSnapshot::read(directory.path())
                    .unwrap()
                    .redaction_secrets()
                    .is_empty()
            );
        }
    }

    #[test]
    fn authentication_snapshot_rejects_creation_removal_and_credential_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let absent = AuthSnapshot::read(directory.path()).unwrap();
        absent.unchanged(directory.path()).unwrap();
        let original = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-original"}"#;
        std::fs::write(&path, original).unwrap();
        assert!(absent.unchanged(directory.path()).is_err());
        let snapshot = AuthSnapshot::read(directory.path()).unwrap();
        snapshot.unchanged(directory.path()).unwrap();
        for changed in [
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-replacement"}"#,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic-cloud-token"}}"#,
            r#"{"auth_mode":"apikey", "OPENAI_API_KEY":"synthetic-original"}"#,
            "{synthetic-invalid-secret",
        ] {
            std::fs::write(&path, changed).unwrap();
            let error = snapshot
                .unchanged(directory.path())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("synthetic"));
        }
        std::fs::remove_file(path).unwrap();
        assert!(snapshot.unchanged(directory.path()).is_err());
    }

    #[test]
    fn authentication_snapshot_rejects_invalid_encoding_shapes_and_oversized_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        for bytes in [vec![0xff], b"[]".to_vec(), vec![b' '; 1024 * 1024 + 1]] {
            std::fs::write(&path, bytes).unwrap();
            assert!(AuthSnapshot::read(directory.path()).is_err());
        }
    }

    #[test]
    fn authentication_inspection_rejects_symlinks_directories_and_fifos() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let config = toml::Value::Table(toml::Table::new());
        let path = directory.path().join("auth.json");
        let outside = directory.path().join("synthetic-auth");
        std::fs::write(&outside, r#"{"auth_mode":"apikey"}"#).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(
            inspect_sources(directory.path(), &config, directory.path(), (false, false)).is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            inspect_sources(directory.path(), &config, directory.path(), (false, false)).is_err()
        );
        std::fs::remove_dir(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // No writer is opened: inspection must fail without waiting for one.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(
            inspect_sources(directory.path(), &config, directory.path(), (false, false)).is_err()
        );
    }
}
