#[allow(dead_code)]
// Some closed proxy controls remain internal until a non-terminal owner is exposed in V3.
pub(crate) mod proxy;
mod pty;
mod runtime_dir;
mod terminal;
mod worker;

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{Config, SessionKernelMode};
use crate::domain::identity::{app_server_source_id, stable_source_id};
use crate::store::Database;
use crate::writer::WriterHandle;

use pty::canonical_executable;
use runtime_dir::RuntimeRoot;
pub use terminal::{OutputEvent, TerminalSnapshot};
pub use worker::{
    CreateFakeSession, CreateSession, ExpectedActiveTurn, InputLeaseView, SessionError,
    SessionRegistry, SessionSource, SessionWorkerHandle, WorkerSnapshot,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionKernelCapabilities {
    pub configured_mode: String,
    pub compiled: bool,
    pub worker_available: bool,
    pub fake_cli_available: bool,
    pub cli_available: bool,
    pub error_code: Option<String>,
}

#[derive(Clone)]
pub struct SessionKernel {
    registry: Option<SessionRegistry>,
    capabilities: SessionKernelCapabilities,
}

impl SessionKernel {
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            registry: None,
            capabilities: SessionKernelCapabilities {
                configured_mode: "off".into(),
                compiled: true,
                worker_available: false,
                fake_cli_available: false,
                cli_available: false,
                error_code: None,
            },
        }
    }

    pub fn from_config(
        config: &Config,
        database: Arc<Database>,
        writer: WriterHandle,
        fingerprint_key: [u8; 32],
    ) -> Result<Self> {
        let cli = canonical_executable(Path::new("codex")).ok();
        let mode = config.controller.session_kernel;
        let registry = if matches!(mode, SessionKernelMode::Preview | SessionKernelMode::Tui) {
            let runtime_path = short_runtime_path(config.database_path())?;
            let runtime_root = RuntimeRoot::prepare(runtime_path)?;
            let sources = config
                .sources
                .iter()
                .filter_map(|source| {
                    source
                        .app_server_socket
                        .as_ref()
                        .map(|socket| SessionSource {
                            source_id: app_server_source_id(socket),
                            store_source_id: stable_source_id(&source.codex_home.to_string_lossy()),
                            codex_home: source.codex_home.clone(),
                            upstream_socket: socket.clone(),
                        })
                })
                .collect();
            Some(SessionRegistry::new_runtime(
                runtime_root,
                config.controller.session_fixture_cli.clone(),
                cli.clone(),
                sources,
                writer,
                database,
                fingerprint_key,
            ))
        } else {
            None
        };
        let fake_cli_available = registry
            .as_ref()
            .is_some_and(SessionRegistry::fixture_available);
        let cli_available = cli.is_some();
        let capabilities = SessionKernelCapabilities {
            configured_mode: mode.as_str().into(),
            compiled: true,
            worker_available: matches!(mode, SessionKernelMode::Preview | SessionKernelMode::Tui),
            fake_cli_available,
            cli_available,
            error_code: if cli_available {
                None
            } else {
                Some("SESSION_KERNEL_CLI_UNAVAILABLE".into())
            },
        };
        Ok(Self {
            registry,
            capabilities,
        })
    }

    #[cfg(test)]
    pub fn preview_for_test(runtime_root: PathBuf, fixture_cli: PathBuf) -> Result<Self> {
        let registry = SessionRegistry::new(
            RuntimeRoot::prepare(runtime_root)?,
            Some(fixture_cli.clone()),
        );
        Ok(Self {
            capabilities: SessionKernelCapabilities {
                configured_mode: "preview".into(),
                compiled: true,
                worker_available: true,
                fake_cli_available: canonical_executable(&fixture_cli).is_ok(),
                cli_available: false,
                error_code: Some("SESSION_KERNEL_CLI_UNAVAILABLE".into()),
            },
            registry: Some(registry),
        })
    }

    pub fn capabilities(&self) -> &SessionKernelCapabilities {
        &self.capabilities
    }

    pub fn registry(&self) -> Result<&SessionRegistry, SessionError> {
        self.registry.as_ref().ok_or_else(|| SessionError {
            code: "CAPABILITY_UNAVAILABLE",
            message: "Session Kernel is disabled".into(),
        })
    }

    pub(crate) fn registry_handle(&self) -> Option<SessionRegistry> {
        self.registry.clone()
    }

    pub async fn shutdown(&self) {
        if let Some(registry) = &self.registry {
            registry.shutdown_all().await;
        }
    }
}

fn short_runtime_path(database_path: &Path) -> Result<std::path::PathBuf> {
    let canonical_parent = database_path
        .parent()
        .context("Observer database has no parent for Session Kernel runtime")?;
    let identity = format!(
        "{}\0{}",
        canonical_parent.display(),
        database_path.display()
    );
    let digest = blake3::hash(identity.as_bytes()).to_hex().to_string();
    #[cfg(unix)]
    let root = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let root = std::env::temp_dir();
    Ok(root.join(format!("codex-observer-{}", &digest[..16])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn feature_off_does_not_create_runtime_or_expose_registry() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.storage.database = temp.path().join("observer.sqlite");
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        let kernel = SessionKernel::from_config(&config, database, writer, [7; 32])?;
        assert_eq!(kernel.capabilities().configured_mode, "off");
        let error = kernel
            .registry()
            .err()
            .expect("feature off must fail closed");
        assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
        assert!(!temp.path().join("session-runtime").exists());
        Ok(())
    }
}
