use super::app_ref::AppRef;
use crate::{
    command::CommandRunner,
    output::OutputLayout,
    process::{BoundedCommand, HealthCheck},
};
use anyhow::Context;
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use tempfile::TempDir;

/// Owns both desktop configuration and the home used by Flatpak's per-app data.
pub struct Workspace {
    _directory: TempDir,
    pub home: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl Workspace {
    pub fn prepare() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir().context("creating capture workspace")?;
        let home = directory.path().to_path_buf();
        for name in ["data", "cache", "config", "state", "runtime", ".var/app"] {
            fs::create_dir_all(home.join(name))?;
        }
        fs::set_permissions(home.join("runtime"), fs::Permissions::from_mode(0o700))?;
        let mut env = vec![
            (
                "PATH".into(),
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()),
            ),
            ("HOME".into(), home.clone().into_os_string()),
        ];
        for (key, value) in [
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "en_US.UTF-8"),
            ("LANGUAGE", "en"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            ("GSETTINGS_BACKEND", "keyfile"),
            ("LIBGL_ALWAYS_SOFTWARE", "1"),
            ("GSK_RENDERER", "cairo"),
            ("GTK_A11Y", "atspi"),
            ("QT_LINUX_ACCESSIBILITY_ALWAYS_ON", "1"),
            ("QT_QPA_PLATFORM", "wayland"),
            ("GDK_BACKEND", "wayland"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ] {
            env.push((key.into(), value.into()));
        }
        for (key, directory) in [
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            env.push((key.into(), home.join(directory).into_os_string()));
        }
        Ok(Self {
            _directory: directory,
            home,
            env,
        })
    }

    pub fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(self.env.iter().cloned());
        command
    }

    pub fn installer_runner(&self, layout: &OutputLayout) -> CommandRunner {
        self.env.iter().fold(
            CommandRunner::new(&layout.runner_log).with_clean_env(),
            |runner, (key, value)| runner.with_env(key.clone(), value.clone()),
        )
    }

    /// Future data imports run here, after installation and before the desktop/app.
    pub fn prepare_app_data(&self, app_ref: &AppRef) -> anyhow::Result<()> {
        fs::create_dir_all(self.home.join(".var/app").join(app_ref.id()))?;
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // A document portal can leave a disconnected FUSE mount after its bus exits.
        let _ = self
            .command("fusermount3")
            .arg("-uz")
            .arg(self.home.join("runtime/doc"))
            .output_before_checked(
                Instant::now() + Duration::from_secs(1),
                &HealthCheck::default(),
            );
    }
}
