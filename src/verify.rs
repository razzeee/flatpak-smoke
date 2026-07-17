use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use tempfile::TempDir;

use crate::{
    cli::{VerifyBundleArgs, VerifyCommonArgs, VerifyRepoArgs},
    command::CommandRunner,
    installer::{ArtifactInstaller, InstallError},
    output::OutputLayout,
    result::{Artifact, FailureReason, RunResult, Timings},
    session::SessionRunner,
    tools,
};

pub fn verify_bundle(args: VerifyBundleArgs) -> anyhow::Result<()> {
    let VerifyBundleArgs { bundle, common } = args;
    tools::check_path(&bundle, "bundle")?;
    let allow_network_remotes = common.allow_network_remotes;
    let artifact = Artifact::bundle(&bundle);
    let bundle_for_flatpak = fs::canonicalize(&bundle)
        .with_context(|| format!("canonicalizing bundle '{}'", bundle.display()))?;
    verify(common, artifact, move |installer| {
        if let Err(error) = installer.add_flathub_if_allowed(allow_network_remotes) {
            return Err((
                None,
                InstallError::new(FailureReason::DependencyFailed, error.to_string()),
            ));
        }
        match installer.install_bundle(&bundle_for_flatpak) {
            Ok(()) => infer_single_installed_app_ref(installer).map_err(|error| {
                (
                    None,
                    InstallError::new(FailureReason::InternalError, error.to_string()),
                )
            }),
            Err(error) => Err((None, error)),
        }
    })
}

pub fn verify_repo(args: VerifyRepoArgs) -> anyhow::Result<()> {
    let VerifyRepoArgs {
        repo,
        app_ref,
        common,
    } = args;
    tools::check_path(&repo, "repo")?;
    validate_app_ref(&app_ref)?;
    let allow_network_remotes = common.allow_network_remotes;
    let artifact = Artifact::repo(&repo);
    let repo_for_flatpak = fs::canonicalize(&repo)
        .with_context(|| format!("canonicalizing repo '{}'", repo.display()))?;
    verify(common, artifact, move |installer| {
        if let Err(error) = installer.add_flathub_if_allowed(allow_network_remotes) {
            return Err((
                Some(app_ref.clone()),
                InstallError::new(FailureReason::DependencyFailed, error.to_string()),
            ));
        }
        installer
            .install_repo_ref(&repo_for_flatpak, &app_ref)
            .map(|()| app_ref.clone())
            .map_err(|error| (Some(app_ref.clone()), error))
    })
}

fn verify<F>(common: VerifyCommonArgs, artifact: Artifact, install: F) -> anyhow::Result<()>
where
    F: FnOnce(&ArtifactInstaller<'_>) -> Result<String, (Option<String>, InstallError)>,
{
    validate_screenshot_name(&common.screenshot_name)?;
    let started = Instant::now();
    let layout = OutputLayout::prepare(&common.output, common.force)?;

    if let Err(error) = tools::ensure_required_tools() {
        let result = RunResult::failed(
            None,
            artifact,
            Timings {
                install: None,
                launch_to_window: None,
                total: started.elapsed().as_millis(),
            },
            Vec::new(),
            FailureReason::InternalError,
            error.to_string(),
        );
        layout.write_result(&result)?;
        bail!("verification failed: {error}");
    }

    let workspace = TempDir::new().context("creating temporary run workspace")?;
    let env = isolated_flatpak_env(workspace.path())?;
    let runner = runner_with_env(&layout, &env);
    layout.append_runner_log(format!("workspace: {}", workspace.path().display()))?;

    let overall_deadline = started + common.overall_timeout;
    let installer = ArtifactInstaller::new(&runner, overall_deadline);
    let install_started = Instant::now();
    let app_ref = match install(&installer) {
        Ok(app_ref) => app_ref,
        Err((app_ref, error)) => {
            let message = error.message;
            let result = RunResult::failed(
                app_ref,
                artifact,
                Timings {
                    install: Some(install_started.elapsed().as_millis()),
                    launch_to_window: None,
                    total: started.elapsed().as_millis(),
                },
                Vec::new(),
                error.reason,
                message.clone(),
            );
            layout.write_result(&result)?;
            bail!("verification failed: {message}");
        }
    };
    let install_ms = install_started.elapsed().as_millis();

    if started.elapsed() >= common.overall_timeout {
        let result = RunResult::failed(
            Some(app_ref),
            artifact,
            Timings {
                install: Some(install_ms),
                launch_to_window: None,
                total: started.elapsed().as_millis(),
            },
            Vec::new(),
            FailureReason::InternalError,
            "overall timeout elapsed before app launch".to_string(),
        );
        layout.write_result(&result)?;
        bail!("verification failed: overall timeout elapsed before app launch");
    }

    let session = SessionRunner::new(
        &layout,
        env,
        common.display_timeout,
        common.window_timeout,
        common.screenshot_timeout,
        overall_deadline,
    );
    let session_result = session.launch_wait_and_capture(&app_ref, &common.screenshot_name);

    match session_result {
        Ok(success) => {
            let result = RunResult::passed(
                app_ref,
                artifact,
                Timings {
                    install: Some(install_ms),
                    launch_to_window: Some(success.launch_to_window_ms),
                    total: started.elapsed().as_millis(),
                },
                success.screenshot_paths,
            );
            layout.write_result(&result)?;
            Ok(())
        }
        Err(error) => {
            let screenshots = error.screenshots;
            let reason = error.reason;
            let message = error.message;
            let result = RunResult::failed(
                Some(app_ref),
                artifact,
                Timings {
                    install: Some(install_ms),
                    launch_to_window: None,
                    total: started.elapsed().as_millis(),
                },
                screenshots,
                reason,
                message.clone(),
            );
            layout.write_result(&result)?;
            bail!("verification failed: {message}");
        }
    }
}

fn isolated_flatpak_env(workspace: &Path) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let dirs = ["data", "cache", "config", "state", "runtime"];
    for dir in dirs {
        fs::create_dir_all(workspace.join(dir))
            .with_context(|| format!("creating workspace directory '{dir}'"))?;
    }
    fs::set_permissions(workspace.join("runtime"), fs::Permissions::from_mode(0o700))
        .context("setting XDG_RUNTIME_DIR permissions")?;
    write_weston_config(workspace)?;
    write_portal_config(workspace)?;

    Ok(vec![
        (
            OsString::from("XDG_DATA_HOME"),
            workspace.join("data").into_os_string(),
        ),
        (
            OsString::from("XDG_CACHE_HOME"),
            workspace.join("cache").into_os_string(),
        ),
        (
            OsString::from("XDG_CONFIG_HOME"),
            workspace.join("config").into_os_string(),
        ),
        (
            OsString::from("XDG_STATE_HOME"),
            workspace.join("state").into_os_string(),
        ),
        (
            OsString::from("XDG_RUNTIME_DIR"),
            workspace.join("runtime").into_os_string(),
        ),
    ])
}

fn write_weston_config(workspace: &Path) -> anyhow::Result<()> {
    fs::write(
        workspace.join("config/weston.ini"),
        "[core]\nidle-time=0\nrenderer=pixman\n\n[shell]\nlocking=false\npanel-position=none\nbackground-color=0xff202020\n",
    )
    .context("writing weston config")?;
    Ok(())
}

fn write_portal_config(workspace: &Path) -> anyhow::Result<()> {
    let config_dir = workspace.join("config/xdg-desktop-portal");
    fs::create_dir_all(&config_dir).context("creating xdg-desktop-portal config directory")?;
    fs::write(
        config_dir.join("portals.conf"),
        "[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.Secret=gnome-keyring\n",
    )
    .context("writing xdg-desktop-portal config")?;
    Ok(())
}

fn infer_single_installed_app_ref(installer: &ArtifactInstaller<'_>) -> anyhow::Result<String> {
    let refs = installer.list_installed_app_refs()?;
    match refs.as_slice() {
        [app_ref] => Ok(normalize_app_ref(app_ref)),
        [] => bail!("bundle installed but no app ref could be inferred"),
        refs => bail!(
            "bundle installed but app ref was ambiguous: {}",
            refs.join(", ")
        ),
    }
}

fn normalize_app_ref(app_ref: &str) -> String {
    if app_ref.starts_with("app/") {
        app_ref.to_string()
    } else {
        format!("app/{app_ref}")
    }
}

fn runner_with_env(layout: &OutputLayout, env: &[(OsString, OsString)]) -> CommandRunner {
    env.iter().fold(
        CommandRunner::new(&layout.runner_log),
        |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
    )
}

fn validate_screenshot_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("--screenshot-name cannot be empty");
    }
    if PathBuf::from(name).components().count() != 1 {
        bail!("--screenshot-name must be a filename, not a path");
    }
    if name.starts_with('-') {
        bail!("--screenshot-name cannot start with '-'");
    }
    if !name.ends_with(".png") {
        bail!("--screenshot-name must end with .png");
    }
    Ok(())
}

fn validate_app_ref(app_ref: &str) -> anyhow::Result<()> {
    if app_ref.trim().is_empty() {
        bail!("app ref cannot be empty");
    }
    if app_ref.starts_with('-') {
        bail!("app ref cannot start with '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_screenshot_paths() {
        assert!(validate_screenshot_name("nested/window.png").is_err());
        assert!(validate_screenshot_name("window.jpg").is_err());
        assert!(validate_screenshot_name("-window.png").is_err());
        assert!(validate_screenshot_name("window.png").is_ok());
    }

    #[test]
    fn rejects_leading_dash_app_refs() {
        assert!(validate_app_ref("--command=sh").is_err());
        assert!(validate_app_ref("app/org.example.App/x86_64/stable").is_ok());
    }

    #[test]
    fn normalizes_flatpak_list_app_refs() {
        assert_eq!(
            normalize_app_ref("org.example.App/x86_64/stable"),
            "app/org.example.App/x86_64/stable"
        );
        assert_eq!(
            normalize_app_ref("app/org.example.App/x86_64/stable"),
            "app/org.example.App/x86_64/stable"
        );
    }

    #[test]
    fn isolated_env_points_under_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let env = isolated_flatpak_env(temp.path()).unwrap();
        assert!(
            env.iter()
                .any(|(key, value)| key == "XDG_DATA_HOME"
                    && value.to_string_lossy().contains("data"))
        );
        assert!(temp.path().join("runtime").is_dir());
        assert!(
            temp.path()
                .join("config/xdg-desktop-portal/portals.conf")
                .is_file()
        );
        assert!(temp.path().join("config/weston.ini").is_file());
        assert_eq!(
            temp.path()
                .join("runtime")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
