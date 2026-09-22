//! Product-owned defaults, independent of the native CLI's CODEX_HOME.
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct WorkbenchPaths {
    pub root: PathBuf,
}
impl WorkbenchPaths {
    pub fn discover() -> io::Result<Self> {
        Self::from_user_home(&user_home()?)
    }

    /// Explicit home injection keeps fixtures out of the real user's files.
    pub fn from_user_home(home: &Path) -> io::Result<Self> {
        if !home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "user home must be an absolute directory",
            ));
        }
        let home = home.canonicalize()?;
        if !home.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(Self {
            root: home.join(".codex-web"),
        })
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn history_dir(&self) -> PathBuf {
        self.root.join("history")
    }
}

pub(crate) fn user_home() -> io::Result<PathBuf> {
    environment_home(cfg!(windows), |name| std::env::var_os(name))
}

fn environment_home(
    windows: bool,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> io::Result<PathBuf> {
    let variable = if windows { "USERPROFILE" } else { "HOME" };
    let home = lookup(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{variable} must identify an absolute user home directory"),
            )
        })?;
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_homes_share_the_layout_and_ignore_native_codex_home() {
        let tmp = tempfile::tempdir().unwrap();
        let unix = tmp.path().join("unix-user");
        let windows = tmp.path().join("windows-user");
        std::fs::create_dir(&unix).unwrap();
        std::fs::create_dir(&windows).unwrap();
        let lookup = |key: &str| match key {
            "HOME" => Some(unix.clone().into_os_string()),
            "USERPROFILE" => Some(windows.clone().into_os_string()),
            "CODEX_HOME" => panic!("workbench defaults must not use CODEX_HOME"),
            _ => None,
        };
        for (platform, home) in [(false, &unix), (true, &windows)] {
            let resolved = environment_home(platform, lookup).unwrap();
            assert_eq!(&resolved, home);
            let paths = WorkbenchPaths::from_user_home(&resolved).unwrap();
            let root = home.canonicalize().unwrap().join(".codex-web");
            assert_eq!(paths.config_dir(), root.join("config"));
            assert_eq!(paths.history_dir(), root.join("history"));
            assert!(!root.exists(), "resolving defaults must not write files");
        }
    }

    #[test]
    fn missing_empty_or_relative_home_never_falls_back_to_cwd_or_codex_home() {
        for windows in [false, true] {
            for value in [None, Some(OsString::new()), Some("relative".into())] {
                assert!(environment_home(windows, |_| value.clone()).is_err());
            }
            assert!(
                environment_home(windows, |key| (key == "CODEX_HOME")
                    .then(|| "/native".into()))
                .is_err()
            );
        }
    }
}
