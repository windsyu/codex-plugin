mod owned_app_server;
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
use serde_json::json;

use crate::config::{Config, SessionKernelMode};
use crate::domain::identity::{session_source_id, stable_source_id};
use crate::store::Database;
use crate::writer::WriterHandle;

pub(crate) use owned_app_server::run_guard as run_owned_app_server_guard;
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
            let default_cwd = std::fs::canonicalize(std::env::current_dir()?)?;
            let source_epoch = uuid::Uuid::new_v4().to_string();
            let sources: Vec<_> = config
                .sources
                .iter()
                .map(|source| {
                    let store_source_id = stable_source_id(&source.codex_home.to_string_lossy());
                    SessionSource {
                        source_id: session_source_id(&store_source_id),
                        source_epoch: source_epoch.clone(),
                        supervisor_version: 1,
                        store_source_id,
                        codex_home: source.codex_home.clone(),
                        default_cwd: default_cwd.clone(),
                        #[cfg(test)]
                        test_upstream_socket: None,
                    }
                })
                .collect();
            for source in &sources {
                let stable_identity = format!("gateway-owned-session:{}", source.store_source_id);
                writer.upsert_source_kind(
                    &source.source_id,
                    "session_runtime",
                    &stable_identity,
                    &json!({
                        "ownership": "gateway_owned_session",
                        "storeSourceId": source.store_source_id,
                    }),
                    "ready",
                )?;
                writer.open_source_epoch(&source.source_id, &source.source_epoch)?;
            }
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

    #[cfg(test)]
    pub fn runtime_for_test(
        runtime_root: PathBuf,
        real_cli: PathBuf,
        sources: Vec<SessionSource>,
        writer: WriterHandle,
        database: Arc<Database>,
    ) -> Result<Self> {
        let registry = SessionRegistry::new_runtime(
            RuntimeRoot::prepare(runtime_root)?,
            None,
            Some(real_cli),
            sources,
            writer,
            database,
            [7; 32],
        );
        Ok(Self {
            capabilities: SessionKernelCapabilities {
                configured_mode: "tui".into(),
                compiled: true,
                worker_available: true,
                fake_cli_available: false,
                cli_available: true,
                error_code: None,
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

    pub async fn shutdown(&self) {
        if let Some(registry) = &self.registry {
            registry.shutdown_all().await;
        }
    }
}

pub(crate) fn codex_cli_available() -> Option<std::path::PathBuf> {
    canonical_executable(Path::new("codex")).ok()
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
    // Keep enough room for `/worker-<uuid>/upstream-app-server.sock` under
    // macOS's 104-byte sockaddr_un.sun_path limit.
    Ok(root.join(format!("co-{}", &digest[..12])))
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
        let kernel = SessionKernel::from_config(&config, database.clone(), writer, [7; 32])?;
        assert_eq!(kernel.capabilities().configured_mode, "off");
        let error = kernel
            .registry()
            .err()
            .expect("feature off must fail closed");
        assert_eq!(error.code, "CAPABILITY_UNAVAILABLE");
        assert!(!temp.path().join("session-runtime").exists());
        Ok(())
    }

    #[test]
    fn configured_stores_share_one_gateway_startup_generation() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.storage.database = temp.path().join("observer.sqlite");
        config.controller.session_kernel = SessionKernelMode::Tui;
        config.sources = ["first", "second"]
            .into_iter()
            .map(|name| {
                let codex_home = temp.path().join(name);
                std::fs::create_dir(&codex_home)?;
                Ok(crate::config::SourceConfig {
                    name: name.into(),
                    codex_home,
                    scan_interval_seconds: 30,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let database = Arc::new(Database::open(&config.storage.database)?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        let kernel = SessionKernel::from_config(&config, database.clone(), writer, [7; 32])?;
        let sources = kernel.registry()?.session_sources();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_epoch, sources[1].source_epoch);
        assert_ne!(sources[0].source_id, sources[1].source_id);
        for source in sources {
            let connection = database.connect()?;
            let registered: i64 = connection.query_row(
                "SELECT COUNT(*) FROM source_epochs e JOIN sources s ON s.source_id=e.source_id
                 WHERE e.source_id=?1 AND e.epoch_id=?2 AND s.kind='session_runtime'",
                rusqlite::params![source.source_id, source.source_epoch],
                |row| row.get(0),
            )?;
            assert_eq!(registered, 1);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn runtime_socket_path_fits_the_macos_unix_socket_limit() -> Result<()> {
        let path = short_runtime_path(Path::new("/private/tmp/observer-data/observer.sqlite"))?
            .join(format!("worker-{}", uuid::Uuid::nil()))
            .join("upstream-app-server.sock");
        assert!(
            path.as_os_str().len() < 104,
            "owned App Server socket path is too long: {} bytes ({})",
            path.as_os_str().len(),
            path.display()
        );
        Ok(())
    }
}
