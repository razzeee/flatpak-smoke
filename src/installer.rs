use std::{ffi::OsString, fs, path::Path, time::Instant};

use anyhow::bail;

use crate::{
    command::CommandRunner,
    result::{FailureReason, InstalledRef},
};

/// With explicit dependency inputs, host installations must not satisfy dependencies
/// behind our backs. Both installation and launch use these same private paths.
pub fn isolate_system_installations(
    workspace: &Path,
    env: &mut Vec<(OsString, OsString)>,
) -> anyhow::Result<()> {
    for (key, directory) in [
        ("FLATPAK_SYSTEM_DIR", "system-flatpak"),
        ("FLATPAK_CONFIG_DIR", "flatpak-config"),
    ] {
        let path = workspace.join(directory);
        fs::create_dir_all(&path)?;
        env.push((key.into(), path.into_os_string()));
    }
    Ok(())
}

pub struct ArtifactInstaller<'a> {
    runner: &'a CommandRunner,
    deadline: Instant,
}

impl<'a> ArtifactInstaller<'a> {
    pub fn new(runner: &'a CommandRunner, deadline: Instant) -> Self {
        Self { runner, deadline }
    }

    pub fn add_dependency_repo(&self, repo: Option<&Path>) -> anyhow::Result<()> {
        let Some(repo) = repo else { return Ok(()) };
        let repo = repo.canonicalize()?;
        anyhow::ensure!(
            repo.join("config").is_file() && repo.join("summary").is_file(),
            "dependency repo must be a published Flatpak repository with config and summary (run flatpak build-update-repo)"
        );
        let output = self.runner.run(
            "flatpak",
            [
                OsString::from("remote-add"),
                OsString::from("--user"),
                OsString::from("--no-gpg-verify"),
                OsString::from("--no-follow_redirect"),
                OsString::from("--prio=100"),
                OsString::from("flatpak-smoke-dependencies"),
                repo.into_os_string(),
            ],
            self.deadline,
        )?;
        anyhow::ensure!(
            output.status.success(),
            "adding dependency repo: {}",
            output.stderr.trim()
        );
        Ok(())
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
            self.deadline,
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
                self.deadline,
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
            self.deadline,
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

    /// Snapshot the app and runtime refs visible in this phase's private installation.
    /// A failed commit lookup is represented as `None`, not silently omitted.
    pub fn phase_refs(&self, app_ref: &str) -> (InstalledRef, Option<Vec<InstalledRef>>) {
        let app = self.installed_ref(app_ref);
        let runtimes = self
            .runner
            .run(
                "flatpak",
                ["list", "--user", "--runtime", "--all", "--columns=ref"],
                self.deadline,
            )
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                output
                    .stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(|reference| self.installed_ref(reference))
                    .collect()
            });
        (app, runtimes)
    }

    fn installed_ref(&self, reference: &str) -> InstalledRef {
        let commit = self
            .runner
            .run(
                "flatpak",
                ["info", "--user", "--show-commit", reference],
                self.deadline,
            )
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout.trim().to_owned())
            .filter(|commit| !commit.is_empty());
        InstalledRef {
            reference: reference.to_owned(),
            commit,
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
                self.deadline,
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
                self.deadline,
            )
            .map_err(|error| InstallError::new(FailureReason::InstallFailed, error.to_string()))?;

        if install_output.status.success() {
            Ok(())
        } else {
            Err(classify_install_error(install_output.stderr))
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
#[path = "installer/tests.rs"]
mod integration_tests;

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
