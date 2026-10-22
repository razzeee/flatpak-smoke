use super::{app_ref::AppRef, recipe::Recipe};
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
    path::{Path, PathBuf},
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

#[derive(Debug)]
pub struct PreparedAppData {
    pub launch_args: Vec<String>,
    pub input_digest: String,
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

    pub fn prepare_app_data(
        &self,
        app_ref: &AppRef,
        recipe: &Recipe,
        recipe_path: &Path,
        recipe_bytes: &[u8],
        deadline: Instant,
    ) -> anyhow::Result<PreparedAppData> {
        let mut digest = super::setup::InputDigest::new(recipe_bytes);
        let profile = self.home.join(".var/app").join(app_ref.id());
        fs::create_dir_all(&profile)?;
        for entry in &recipe.setup.files {
            let source = recipe_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&entry.source);
            digest.entry();
            super::setup::copy(
                &source,
                &profile.join(&entry.destination),
                &entry.source,
                &mut digest,
                deadline,
            )
            .with_context(|| {
                format!(
                    "seeding {} from {}",
                    entry.destination.display(),
                    source.display()
                )
            })?;
        }
        let launch_args = recipe
            .launch
            .args
            .iter()
            .map(|arg| super::setup::expand(arg, &profile))
            .collect::<anyhow::Result<_>>()?;
        Ok(PreparedAppData {
            launch_args,
            input_digest: digest.finish(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screenshot::recipe::Recipe;

    #[test]
    fn input_digest_tracks_seeded_fixtures_independent_of_tree_location_and_creation_order() {
        let bytes = b"version: 1\nsetup:\n  files:\n    - {source: gimprc, destination: config/gimprc}\n    - {source: project, destination: data/project}\nsteps: [{capture: {name: main, caption: Main}}]\n";
        let recipe = Recipe::parse(bytes).unwrap();
        let app = AppRef::parse("app/org.example.Test/x86_64/stable").unwrap();
        let seed = |root: &Path| {
            let workspace = Workspace::prepare().unwrap();
            let prepared = workspace
                .prepare_app_data(
                    &app,
                    &recipe,
                    &root.join("recipe.yml"),
                    bytes,
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            (workspace, prepared.input_digest)
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, names) in [(first.path(), ["a", "z"]), (second.path(), ["z", "a"])] {
            fs::create_dir_all(root.join("project/nested")).unwrap();
            fs::write(root.join("gimprc"), "initial settings").unwrap();
            for name in names {
                fs::write(root.join("project/nested").join(name), name).unwrap();
            }
        }
        let (snapshot, original) = seed(first.path());
        assert!(original.starts_with("v1:sha256:"));
        assert_eq!(original.len(), 74);
        assert_eq!(original, seed(second.path()).1);

        fs::write(first.path().join("gimprc"), "changed settings").unwrap();
        assert_ne!(original, seed(first.path()).1);
        // The digest belongs to the copied snapshot, not subsequent source changes.
        assert_eq!(
            fs::read(
                snapshot
                    .home
                    .join(".var/app/org.example.Test/config/gimprc")
            )
            .unwrap(),
            b"initial settings"
        );
        fs::write(first.path().join("gimprc"), "initial settings").unwrap();
        assert_eq!(original, seed(first.path()).1);
        fs::write(first.path().join("project/nested/a"), "changed nested file").unwrap();
        assert_ne!(original, seed(first.path()).1);
        fs::write(first.path().join("project/nested/a"), "a").unwrap();
        fs::rename(
            first.path().join("project/nested/a"),
            first.path().join("project/nested/b"),
        )
        .unwrap();
        assert_ne!(original, seed(first.path()).1);
        fs::rename(
            first.path().join("project/nested/b"),
            first.path().join("project/nested/a"),
        )
        .unwrap();
        fs::create_dir(first.path().join("project/empty")).unwrap();
        assert_ne!(original, seed(first.path()).1);
    }

    #[test]
    fn recipe_seeds_a_fresh_profile_and_resolves_launch_arguments() {
        let fixtures = tempfile::tempdir().unwrap();
        fs::create_dir_all(fixtures.path().join("project/nested")).unwrap();
        fs::write(
            fixtures.path().join("project/nested/document.txt"),
            "Sample content",
        )
        .unwrap();
        fs::write(fixtures.path().join("settings.ini"), "welcome=false").unwrap();
        let path = fixtures.path().join("recipe.yml");
        fs::write(&path, r#"
version: 1
setup:
  files:
    - {source: project, destination: data/example project}
    - {source: settings.ini, destination: config/settings.ini}
launch:
  args: ["--open", "${APP_DATA}/example project/nested/document.txt", "${APP_CONFIG}/settings.ini", "$(literal)"]
steps: [{capture: {name: main, caption: Sample content}}]
"#).unwrap();
        let recipe = Recipe::parse(&fs::read(&path).unwrap()).unwrap();
        let app = AppRef::parse("app/org.example.Test/x86_64/stable").unwrap();
        for _ in 0..2 {
            let workspace = Workspace::prepare().unwrap();
            let args = workspace
                .prepare_app_data(
                    &app,
                    &recipe,
                    &path,
                    &fs::read(&path).unwrap(),
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            let profile = workspace.home.join(".var/app/org.example.Test");
            assert_eq!(
                fs::read_to_string(profile.join("data/example project/nested/document.txt"))
                    .unwrap(),
                "Sample content"
            );
            assert_eq!(
                args.launch_args,
                vec![
                    "--open".to_string(),
                    format!(
                        "{}/data/example project/nested/document.txt",
                        profile.display()
                    ),
                    format!("{}/config/settings.ini", profile.display()),
                    "$(literal)".to_string()
                ]
            );
            fs::write(
                profile.join("data/example project/nested/document.txt"),
                "Modified by app",
            )
            .unwrap();
        }
        assert_eq!(
            fs::read_to_string(fixtures.path().join("project/nested/document.txt")).unwrap(),
            "Sample content"
        );
    }

    #[test]
    fn setup_rejects_links_missing_sources_conflicts_and_expired_deadlines() {
        use std::os::unix::fs::symlink;
        let fixtures = tempfile::tempdir().unwrap();
        fs::create_dir(fixtures.path().join("real")).unwrap();
        fs::write(fixtures.path().join("real/file"), "original").unwrap();
        symlink("real", fixtures.path().join("link")).unwrap();
        symlink("real/file", fixtures.path().join("file-link")).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(fixtures.path().join("socket")).unwrap();
        let app = AppRef::parse("app/org.example.Test/x86_64/stable").unwrap();
        let path = fixtures.path().join("recipe.yml");
        for (source, duplicate, expired, message) in [
            ("link/file", false, false, "symlink"),
            ("file-link", false, false, "symlink"),
            ("socket", false, false, "regular file or directory"),
            ("missing", false, false, "missing"),
            ("real/file", true, false, "exists"),
            ("real", false, true, "deadline"),
        ] {
            let entry = format!("    - {{source: {source}, destination: data/file}}\n");
            fs::write(&path, format!("version: 1\nsetup:\n  files:\n{entry}{}steps: [{{capture: {{name: main, caption: Main view}}}}]\n", if duplicate { &entry } else { "" })).unwrap();
            let recipe = Recipe::parse(&fs::read(&path).unwrap()).unwrap();
            let workspace = Workspace::prepare().unwrap();
            let deadline = Instant::now()
                + if expired {
                    Duration::ZERO
                } else {
                    Duration::from_secs(5)
                };
            let error = workspace
                .prepare_app_data(&app, &recipe, &path, &fs::read(&path).unwrap(), deadline)
                .unwrap_err();
            assert!(format!("{error:#}").contains(message), "{error:#}");
        }
    }
}
