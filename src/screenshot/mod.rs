mod app_ref;
mod author;
mod desktop;
mod gnome;
mod images;
mod manifest;
mod recipe;
mod setup;
mod snapshot;
mod workflow;
mod workspace;

use crate::{
    cli::{ScreenshotArgs, ScreenshotBundleArgs, ScreenshotRepoArgs},
    installer::ArtifactInstaller,
    output::OutputLayout,
    process::{BoundedCommand, HealthCheck},
    result::{
        Artifact, FailureCategory, FailureReason, PhaseMetadata, RecipeMetadata, RunResult, Timings,
    },
    session::AppBackend,
    tools,
};
use anyhow::Context;
use app_ref::AppRef;
use desktop::Desktop;
use md5::{Digest, Md5};
use std::{
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct CaptureFailure {
    reason: FailureReason,
    category: Option<FailureCategory>,
    message: String,
}
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for CaptureFailure {}
fn failure(reason: FailureReason, message: impl Into<String>) -> anyhow::Error {
    CaptureFailure {
        reason,
        category: None,
        message: message.into(),
    }
    .into()
}

fn categorized_failure(
    reason: FailureReason,
    category: FailureCategory,
    message: impl Into<String>,
) -> anyhow::Error {
    CaptureFailure {
        reason,
        category: Some(category),
        message: message.into(),
    }
    .into()
}

fn infrastructure(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<CaptureFailure>().is_some() {
        error
    } else {
        categorized_failure(
            FailureReason::ScreenshotFailed,
            FailureCategory::Infrastructure,
            format!("{error:#}"),
        )
    }
}

enum Source {
    Bundle(PathBuf),
    Repo(PathBuf, AppRef),
}

pub fn bundle(args: ScreenshotBundleArgs) -> anyhow::Result<()> {
    run(args.common, Source::Bundle(args.bundle))
}

pub fn repo(args: ScreenshotRepoArgs) -> anyhow::Result<()> {
    run(
        args.common,
        Source::Repo(args.repo, AppRef::parse(&args.app_ref)?),
    )
}

pub fn author_bundle(args: ScreenshotBundleArgs) -> anyhow::Result<()> {
    author::bundle(args)
}

fn run(common: ScreenshotArgs, source: Source) -> anyhow::Result<()> {
    run_session(&common, &source, None)
}

fn run_session(
    common: &ScreenshotArgs,
    source: &Source,
    mut author: Option<&mut author::Session>,
) -> anyhow::Result<()> {
    let recipe_contents = std::fs::read(&common.recipe).context("reading screenshot recipe")?;
    let mut recipe = if author.is_some() {
        recipe::Recipe::parse_draft(&recipe_contents)?
    } else {
        recipe::Recipe::parse(&recipe_contents)?
    };
    let (path, artifact) = match source {
        Source::Bundle(path) => (path, Artifact::bundle(path)),
        Source::Repo(path, _) => (path, Artifact::repo(path)),
    };
    tools::check_path(path, "artifact")?;
    let absolute_path = path.canonicalize()?;
    let started = Instant::now();
    let deadline = started
        .checked_add(common.overall_timeout)
        .context("overall timeout is too large")?;
    let deadline = author.as_ref().map_or(deadline, |session| session.deadline);
    let layout = OutputLayout::prepare(&common.output, common.force)?;
    let mut manifest = manifest::Manifest::new();
    let mut timings = Timings::default();
    let mut phase_metadata = PhaseMetadata {
        recipe: Some(RecipeMetadata {
            content_hash: format!(
                "md5:{}",
                Md5::digest(&recipe_contents)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            input_digest: None,
        }),
        ..PhaseMetadata::default()
    };
    let outcome = (|| {
        recipe.validate_setup()?;
        check_dependencies(workflow::bounded(deadline, Duration::from_secs(5)))
            .map_err(|error| failure(FailureReason::DependencyFailed, error.to_string()))?;
        let mut workspace = workspace::Workspace::prepare()?;
        if common.dependency_repo.is_some() {
            crate::installer::isolate_system_installations(
                &workspace.home,
                &mut workspace.env,
                &layout.runner_log,
                deadline,
            )
            .map_err(|error| failure(FailureReason::DependencyFailed, error.to_string()))?;
        }
        let workspace = Rc::new(workspace);
        layout.append_runner_log(format!("capture workspace: {}", workspace.home.display()))?;
        let runner = workspace.installer_runner(&layout);
        let installer = ArtifactInstaller::new(&runner, deadline);
        let install_started = Instant::now();
        let installed = (|| {
            installer
                .add_dependency_repo(common.dependency_repo.as_deref())
                .map_err(|error| failure(FailureReason::DependencyFailed, error.to_string()))?;
            installer
                .add_flathub_if_allowed(common.allow_network_remotes)
                .map_err(|error| failure(FailureReason::DependencyFailed, error.to_string()))?;
            match source {
                Source::Bundle(_) => {
                    installer
                        .install_bundle(&absolute_path)
                        .map_err(|error| failure(error.reason, error.message))?;
                    let refs = installer.list_installed_app_refs()?;
                    anyhow::ensure!(
                        refs.len() == 1,
                        "bundle must install exactly one application"
                    );
                    AppRef::from_installed(&refs[0])
                }
                Source::Repo(_, app_ref) => {
                    installer
                        .install_repo_ref(&absolute_path, &app_ref.to_string())
                        .map_err(|error| failure(error.reason, error.message))?;
                    Ok(app_ref.clone())
                }
            }
        })();
        timings.install = Some(install_started.elapsed().as_millis());
        let app_ref = installed?;
        let (app, runtimes) = installer.phase_refs(&app_ref.to_string());
        phase_metadata.app = Some(app);
        phase_metadata.runtimes = runtimes;
        manifest.app_ref = Some(app_ref.to_string());
        let permissions = workspace
            .command("flatpak")
            .args(["info", "--user", "--show-permissions"])
            .arg(app_ref.id())
            .output_before_checked(deadline, &HealthCheck::default())
            .context("reading effective app display permissions")?;
        anyhow::ensure!(
            permissions.status.success(),
            "reading effective app display permissions: {}",
            String::from_utf8_lossy(&permissions.stderr).trim()
        );
        let backend = AppBackend::from_permissions(&String::from_utf8_lossy(&permissions.stdout));
        layout.append_runner_log(format!("selected screenshot display backend: {backend:?}"))?;
        if backend == AppBackend::Xwayland {
            let missing = tools::missing_tools(tools::XWAYLAND_TOOLS);
            anyhow::ensure!(
                missing.is_empty(),
                "X11 screenshot compatibility requires: {}",
                missing.join(", ")
            );
        }
        let prepared = workspace.prepare_app_data(
            &app_ref,
            &recipe,
            &common.recipe,
            &recipe_contents,
            deadline,
        )?;
        phase_metadata
            .recipe
            .as_mut()
            .expect("recipe metadata initialized")
            .input_digest = Some(prepared.input_digest);
        let mut desktop = gnome::Gnome::start(
            workspace.clone(),
            &layout,
            backend,
            workflow::bounded(deadline, common.display_timeout),
        )
        .map_err(|error| {
            if error.downcast_ref::<CaptureFailure>().is_some() {
                error
            } else {
                failure(FailureReason::DisplayStartFailed, format!("{error:#}"))
            }
        })?;
        manifest.desktop_version = Some(desktop.version().to_string());
        let launch_started = Instant::now();
        desktop.launch(
            &app_ref,
            &prepared.launch_args,
            &layout,
            workflow::bounded(deadline, common.window_timeout),
        )?;
        let workflow = workflow::Workflow {
            desktop: &desktop,
            images: images::Images {
                workspace: &workspace,
                logs: &layout.logs_dir,
                health: desktop.health(),
            },
            layout: &layout,
            overall: deadline,
        };
        let result = (|| {
            let selected =
                workflow.initialize(&recipe, workflow::bounded(deadline, common.window_timeout))?;
            timings.launch_to_window = Some(launch_started.elapsed().as_millis());
            let selected =
                workflow.execute(&recipe, selected, common.screenshot_timeout, &mut manifest)?;
            if let Some(author) = author.as_mut() {
                author
                    .interact(
                        &workflow,
                        &mut recipe,
                        &recipe_contents,
                        selected,
                        common,
                        &mut manifest,
                    )
                    .map_err(|error| {
                        if error.downcast_ref::<CaptureFailure>().is_some() {
                            error
                        } else {
                            failure(FailureReason::ScreenshotFailed, format!("{error:#}"))
                        }
                    })?;
            }
            Ok(())
        })();
        // Preserve the workflow's classified failure, especially cancellation.
        // Only a successful workflow needs a final liveness check.
        result.and_then(|()| desktop.check(deadline))
    })();
    timings.total = started.elapsed().as_millis();
    // Report failed actions even when the user chooses to restart.
    // Only a fresh replay can validate the completed recipe.
    let reported_error = outcome.as_ref().err().or_else(|| {
        author
            .as_ref()
            .and_then(|session| session.failed_action.as_ref())
    });
    let mut screenshots: Vec<_> = manifest
        .captures
        .iter()
        .map(|capture| capture.path.clone())
        .collect();
    if reported_error.is_some() && layout.logs_dir.join("last-candidate.png").is_file() {
        screenshots.push("logs/last-candidate.png".into());
    }
    let mut result = match reported_error {
        None => RunResult::passed(
            manifest.app_ref.clone().unwrap_or_default(),
            artifact,
            timings,
            screenshots,
        ),
        Some(error) => RunResult::failed(
            manifest.app_ref.clone(),
            artifact,
            timings,
            screenshots,
            error
                .downcast_ref::<CaptureFailure>()
                .map(|error| error.reason)
                .unwrap_or(FailureReason::InternalError),
            format!("{error:#}"),
        ),
    };
    result.phase_metadata = phase_metadata;
    if let (Some(failure), Some(error)) = (&mut result.failure, reported_error) {
        failure.category = error
            .downcast_ref::<CaptureFailure>()
            .and_then(|error| error.category);
    }
    manifest.write(&common.output.join("screenshots.json"))?;
    layout.write_result(&result)?;
    outcome
}

pub fn doctor() -> anyhow::Result<()> {
    let version = check_dependencies(Instant::now() + Duration::from_secs(5))?;
    let workspace = Rc::new(workspace::Workspace::prepare()?);
    let layout = OutputLayout::prepare(&workspace.home.join("doctor-output"), false)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let desktop = gnome::Gnome::start(workspace, &layout, AppBackend::Wayland, deadline)?;
    desktop.check(deadline)?;
    println!("flatpak-smoke doctor: screenshot tools and native helper ok ({version})");
    Ok(())
}

fn check_dependencies(deadline: Instant) -> anyhow::Result<String> {
    let missing = tools::missing_tools(&[
        "gnome-shell",
        "python3",
        "gsettings",
        "flatpak",
        "dbus-run-session",
        "gnome-keyring-daemon",
        "fusermount3",
        "tesseract",
        "convert",
        "identify",
        "locale",
    ]);
    anyhow::ensure!(
        missing.is_empty(),
        "missing screenshot tools: {}",
        missing.join(", ")
    );
    use crate::process::{BoundedCommand, HealthCheck};
    let version = std::process::Command::new("gnome-shell")
        .arg("--version")
        .output_before_checked(deadline, &HealthCheck::default())?;
    anyhow::ensure!(
        version.status.success(),
        "could not query GNOME Shell version"
    );
    let version = String::from_utf8(version.stdout)?;
    anyhow::ensure!(
        version.trim().starts_with("GNOME Shell 48."),
        "screenshot backend requires GNOME Shell 48; found {}",
        version.trim()
    );
    let gi = std::process::Command::new("python3")
        .args(["-c", "import gi; gi.require_version('Atspi', '2.0'); from gi.repository import Gio, GLib, Atspi"])
        .output_before_checked(deadline, &HealthCheck::default())?;
    anyhow::ensure!(
        gi.status.success(),
        "Python GObject/Gio and AT-SPI bindings are required"
    );
    let schemas = std::process::Command::new("gsettings")
        .arg("list-schemas")
        .output_before_checked(deadline, &HealthCheck::default())?;
    let schemas_text = String::from_utf8_lossy(&schemas.stdout);
    anyhow::ensure!(
        schemas.status.success()
            && ["org.gnome.shell", "org.gnome.desktop.interface"]
                .iter()
                .all(|name| schemas_text.lines().any(|line| line == *name)),
        "GNOME desktop settings schemas are required"
    );
    let locales = std::process::Command::new("locale")
        .arg("-a")
        .output_before_checked(deadline, &HealthCheck::default())?;
    anyhow::ensure!(
        locales.status.success()
            && String::from_utf8_lossy(&locales.stdout)
                .lines()
                .any(|line| matches!(line, "en_US.utf8" | "en_US.UTF-8")),
        "en_US.UTF-8 locale is required"
    );
    let languages = std::process::Command::new("tesseract")
        .arg("--list-langs")
        .output_before_checked(deadline, &HealthCheck::default())?;
    anyhow::ensure!(
        languages.status.success()
            && String::from_utf8_lossy(&languages.stdout)
                .lines()
                .any(|line| line.trim() == "eng"),
        "Tesseract English language data is required"
    );
    Ok(version.trim().to_string())
}
