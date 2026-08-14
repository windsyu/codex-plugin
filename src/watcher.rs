use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, PollWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::config::Config;

/// Owns the platform watcher. Notifications are deliberately reduced to rescan hints.
pub struct RolloutWatcher {
    _watcher: PollWatcher,
}

impl RolloutWatcher {
    pub fn start(config: &Config) -> Result<(Self, mpsc::UnboundedReceiver<PathBuf>)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut watcher = PollWatcher::new(
            move |result: notify::Result<Event>| match result {
                Ok(event) => {
                    for path in event.paths {
                        if is_rollout_hint(&path) {
                            let _ = sender.send(path);
                            break;
                        }
                    }
                }
                Err(error) => tracing::warn!(error = %error, "filesystem watcher error"),
            },
            notify::Config::default().with_poll_interval(Duration::from_millis(500)),
        )
        .context("create filesystem watcher")?;

        let mut watched = 0;
        for source in &config.sources {
            let mut source_watched = false;
            for directory in [
                source.codex_home.join("sessions"),
                source.codex_home.join("archived_sessions"),
            ] {
                if directory.is_dir() {
                    watcher
                        .watch(&directory, RecursiveMode::Recursive)
                        .with_context(|| format!("watch {}", directory.display()))?;
                    watched += 1;
                    source_watched = true;
                }
            }
            if !source_watched && source.codex_home.is_dir() {
                watcher
                    .watch(&source.codex_home, RecursiveMode::Recursive)
                    .with_context(|| format!("watch {}", source.codex_home.display()))?;
                watched += 1;
            }
        }
        tracing::info!(watched_directories = watched, "rollout watcher registered");
        Ok((Self { _watcher: watcher }, receiver))
    }
}

fn is_rollout_hint(path: &std::path::Path) -> bool {
    let value = path.to_string_lossy();
    value.contains("sessions")
        && (value.ends_with(".jsonl")
            || value.ends_with(".jsonl.zst")
            || value.contains("rollout-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn filters_rollout_related_paths() {
        assert!(is_rollout_hint(std::path::Path::new(
            "/tmp/.codex/sessions/2026/08/14/rollout-a.jsonl"
        )));
        assert!(is_rollout_hint(std::path::Path::new(
            "/tmp/.codex/archived_sessions/rollout-a.jsonl.zst"
        )));
        assert!(!is_rollout_hint(std::path::Path::new(
            "/tmp/.codex/config.toml"
        )));
    }

    #[tokio::test]
    async fn emits_rescan_hint_for_new_rollout() -> Result<()> {
        let temp = TempDir::new()?;
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions)?;
        let mut config = Config {
            config_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        config.sources[0].codex_home = temp.path().to_path_buf();
        let (_watcher, mut receiver) = RolloutWatcher::start(&config)?;
        let rollout = sessions.join("rollout-watcher-test.jsonl");
        fs::write(&rollout, b"{}\n")?;
        let hint = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await?
            .context("watcher channel closed")?;
        assert_eq!(hint, rollout);
        Ok(())
    }
}
