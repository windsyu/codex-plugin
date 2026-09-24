//! Stable diagnostics for owner APIs; never serialize arbitrary error chains.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LaunchFailure {
    Executable,
    Version,
    Config,
    Provider,
    Auth,
    ProjectConfig,
    Recording,
    Proxy,
    Terminal,
    Workspace,
    ConfigChanged,
}
impl LaunchFailure {
    pub(crate) const ALL: [Self; 11] = [
        Self::Executable,
        Self::Version,
        Self::Config,
        Self::Provider,
        Self::Auth,
        Self::ProjectConfig,
        Self::Recording,
        Self::Proxy,
        Self::Terminal,
        Self::Workspace,
        Self::ConfigChanged,
    ];
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Executable => "native_cli_not_found",
            Self::Version => "native_cli_version_failed",
            Self::Config => "native_config_invalid",
            Self::Provider => "native_provider_unsupported",
            Self::Auth => "native_auth_unverified",
            Self::ProjectConfig => "native_project_config_invalid",
            Self::Recording => "native_recording_unavailable",
            Self::Proxy => "native_proxy_unavailable",
            Self::Terminal => "native_terminal_unavailable",
            Self::Workspace => "native_workspace_unavailable",
            Self::ConfigChanged => "native_config_changed",
        }
    }
    pub(crate) fn wrap(self, error: impl Into<anyhow::Error>) -> anyhow::Error {
        let error = error.into();
        if error.downcast_ref::<Self>().is_some() {
            error
        } else {
            error.context(self)
        }
    }
    pub(crate) fn from_error(error: &anyhow::Error) -> &'static str {
        error
            .downcast_ref::<Self>()
            .map_or("native_launch_failed", |e| e.code())
    }
}
impl std::fmt::Display for LaunchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LaunchFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_diagnostics_keep_specific_stage_without_error_payloads() {
        let error = LaunchFailure::Auth.wrap(anyhow::anyhow!("synthetic-private-credential"));
        let error = LaunchFailure::Config.wrap(error);
        assert_eq!(LaunchFailure::from_error(&error), "native_auth_unverified");
        assert_eq!(
            LaunchFailure::from_error(&anyhow::anyhow!("synthetic-private-path")),
            "native_launch_failed"
        );
        for failure in LaunchFailure::ALL {
            assert_eq!(
                LaunchFailure::from_error(&failure.wrap(anyhow::anyhow!("private"))),
                failure.code()
            );
        }
    }
}
