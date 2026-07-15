use std::{
    ffi::OsString,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::bail;

use crate::{command::CommandRunner, result::FailureReason};

pub struct ArtifactInstaller<'a> {
    runner: &'a CommandRunner,
    deadline: Instant,
}

impl<'a> ArtifactInstaller<'a> {
    pub fn new(runner: &'a CommandRunner, deadline: Instant) -> Self {
        Self { runner, deadline }
    }

    pub fn add_flathub_if_allowed(&self, allow_network_remotes: bool) -> anyhow::Result<()> {
        if !allow_network_remotes {
            return Ok(());
        }

        let output = self.runner.run(
            "flatpak",
            [
                "remote-add",
                "--user",
                "--if-not-exists",
                "flathub",
                "https://flathub.org/repo/flathub.flatpakrepo",
            ],
            self.remaining_timeout()?,
        )?;

        if output.status.success() {
            Ok(())
        } else {
            bail!("failed to add Flathub remote: {}", output.stderr.trim())
        }
    }

    pub fn install_bundle(&self, bundle: &Path) -> Result<(), InstallError> {
        let output = self
            .runner
            .run(
                "flatpak",
                [
                    OsString::from("install"),
                    OsString::from("--user"),
                    OsString::from("--noninteractive"),
                    bundle.as_os_str().to_os_string(),
                ],
                self.remaining_timeout().map_err(|error| {
                    InstallError::new(FailureReason::InstallFailed, error.to_string())
                })?,
            )
            .map_err(|error| InstallError::new(FailureReason::InstallFailed, error.to_string()))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(classify_install_error(output.stderr))
        }
    }

    pub fn list_installed_app_refs(&self) -> anyhow::Result<Vec<String>> {
        let output = self.runner.run(
            "flatpak",
            ["list", "--user", "--app", "--columns=ref"],
            self.remaining_timeout()?,
        )?;

        if output.status.success() {
            Ok(output
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect())
        } else {
            bail!("flatpak list failed: {}", output.stderr.trim())
        }
    }

    pub fn install_repo_ref(&self, repo: &Path, app_ref: &str) -> Result<(), InstallError> {
        let remote_name = "flatpak-smoke-local";
        let remote_output = self
            .runner
            .run(
                "flatpak",
                [
                    OsString::from("remote-add"),
                    OsString::from("--user"),
                    OsString::from("--if-not-exists"),
                    OsString::from("--no-gpg-verify"),
                    OsString::from(remote_name),
                    repo.as_os_str().to_os_string(),
                ],
                self.remaining_timeout().map_err(|error| {
                    InstallError::new(FailureReason::InstallFailed, error.to_string())
                })?,
            )
            .map_err(|error| InstallError::new(FailureReason::InstallFailed, error.to_string()))?;

        if !remote_output.status.success() {
            return Err(InstallError {
                reason: FailureReason::InstallFailed,
                message: format!("flatpak remote-add failed: {}", remote_output.stderr.trim()),
            });
        }

        let install_output = self
            .runner
            .run(
                "flatpak",
                [
                    "install",
                    "--user",
                    "--noninteractive",
                    remote_name,
                    app_ref,
                ],
                self.remaining_timeout().map_err(|error| {
                    InstallError::new(FailureReason::InstallFailed, error.to_string())
                })?,
            )
            .map_err(|error| InstallError::new(FailureReason::InstallFailed, error.to_string()))?;

        if install_output.status.success() {
            Ok(())
        } else {
            Err(classify_install_error(install_output.stderr))
        }
    }

    fn remaining_timeout(&self) -> anyhow::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("overall timeout elapsed")
        } else {
            Ok(remaining)
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallError {
    pub reason: FailureReason,
    pub message: String,
}

impl InstallError {
    pub fn new(reason: FailureReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

fn classify_install_error(stderr: String) -> InstallError {
    let lower = stderr.to_lowercase();
    let reason = if lower.contains("runtime")
        || lower.contains("dependency")
        || lower.contains("no such ref")
        || lower.contains("no remote refs found")
        || lower.contains("not found")
    {
        FailureReason::DependencyFailed
    } else {
        FailureReason::InstallFailed
    };

    InstallError::new(reason, format!("flatpak install failed: {}", stderr.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_runtime_resolution_failures_as_dependency_failures() {
        let error = classify_install_error(
            "Required runtime org.freedesktop.Platform not found".to_string(),
        );
        assert_eq!(error.reason, FailureReason::DependencyFailed);
    }

    #[test]
    fn classifies_other_install_failures_as_install_failed() {
        let error = classify_install_error("error opening bundle".to_string());
        assert_eq!(error.reason, FailureReason::InstallFailed);
    }
}
