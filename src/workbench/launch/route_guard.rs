//! File-generation fences for a native workspace route. A token refresh may
//! continue only after the official route reader confirms the same destination.
use super::native_route;
use crate::workbench::proxy::{WorkspaceFence, WorkspaceGuard};
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, PartialEq)]
pub(super) struct Snapshot(Vec<(PathBuf, Option<blake3::Hash>)>, (bool, bool));
impl Snapshot {
    pub fn read(paths: &[PathBuf]) -> Result<Self> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut values = Vec::new();
        for path in paths {
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    values.push((path.clone(), None));
                    continue;
                }
                Err(_) => anyhow::bail!("native route source unavailable"),
            };
            ensure!(
                file.metadata()?.is_file(),
                "native route source must be a regular file"
            );
            let mut bytes = Vec::new();
            file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 2 * 1024 * 1024,
                "native route source exceeds bound"
            );
            values.push((path.clone(), Some(blake3::hash(&bytes))));
        }
        Ok(Self(
            values,
            crate::workbench::config_sources::managed_preferences()?,
        ))
    }
    pub fn paths(&self) -> Vec<PathBuf> {
        self.0.iter().map(|(path, _)| path.clone()).collect()
    }
    pub fn unchanged(&self) -> bool {
        Self::read(&self.paths()).is_ok_and(|current| current == *self)
    }
}
impl WorkspaceFence for Snapshot {
    fn unchanged(&self) -> bool {
        self.unchanged()
    }
}

pub(super) struct Guard {
    binary: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    named: Option<String>,
    expected_url: String,
    expected_account: String,
    expected_override: Option<String>,
    generation: tokio::sync::Mutex<Snapshot>,
}
impl Guard {
    pub fn new(
        binary: &Path,
        home: &Path,
        cwd: &Path,
        named: Option<&str>,
        route: &native_route::Route,
    ) -> Arc<Self> {
        let workspace = route.workspace.as_ref().unwrap();
        Arc::new(Self {
            binary: binary.into(),
            home: home.into(),
            cwd: cwd.into(),
            named: named.map(str::to_owned),
            expected_url: route.url.clone(),
            expected_account: workspace.account_id.clone(),
            expected_override: workspace.routing_override.clone(),
            generation: tokio::sync::Mutex::new(route.snapshot.clone()),
        })
    }
}
impl WorkspaceGuard for Guard {
    fn check(&self) -> futures_util::future::BoxFuture<'_, Result<Arc<dyn WorkspaceFence>>> {
        Box::pin(async move {
            let mut generation = self.generation.lock().await;
            if !generation.unchanged() {
                let route =
                    native_route::read(&self.binary, &self.home, &self.cwd, self.named.as_deref())
                        .await?;
                let workspace = route
                    .workspace
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("native workspace route changed"))?;
                ensure!(
                    route.url == self.expected_url
                        && workspace.account_id == self.expected_account
                        && workspace.routing_override == self.expected_override,
                    "native workspace route changed; start a new Run"
                );
                *generation = route.snapshot;
            }
            Ok(Arc::new(generation.clone()) as Arc<dyn WorkspaceFence>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_generation_detects_content_changes_even_when_length_is_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        std::fs::write(&path, "old-token").unwrap();
        let snapshot = Snapshot::read(std::slice::from_ref(&path)).unwrap();
        assert!(snapshot.unchanged());
        std::fs::write(&path, "new-token").unwrap();
        assert!(!snapshot.unchanged());
    }
    #[test]
    fn absent_sources_and_symlink_replacements_are_fenced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let snapshot = Snapshot::read(std::slice::from_ref(&path)).unwrap();
        std::fs::write(&path, "route='changed'").unwrap();
        assert!(!snapshot.unchanged());
        let snapshot = Snapshot::read(std::slice::from_ref(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/dev/null", &path).unwrap();
        assert!(!snapshot.unchanged());
    }
}
