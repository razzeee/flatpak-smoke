//! Forward updates own one workspace across both recipes and the deployment change.
use super::{
    app_ref::AppRef,
    desktop::Desktop,
    failure,
    gnome::Gnome,
    images::Images,
    manifest::Manifest,
    recipe::{Action, Recipe},
    workflow::{Workflow, bounded},
    workspace::Workspace,
};
use crate::{
    cli::UpdateBundleArgs,
    installer::{ArtifactInstaller, isolate_system_installations},
    output::OutputLayout,
    process::{BoundedCommand, HealthCheck},
    result::{
        Artifact, FailureReason, InstalledRef, PhaseMetadata, RecipeMetadata, RunResult, Timings,
        UpdateMetadata,
    },
    session::AppBackend,
};
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::{fs, rc::Rc, time::Instant};

pub fn run(args: UpdateBundleArgs) -> anyhow::Result<()> {
    let started = Instant::now();
    let deadline = started
        .checked_add(args.overall_timeout)
        .context("overall timeout is too large")?;
    let layout = OutputLayout::prepare(&args.output, args.force)?;
    let mut stage = "validate";
    let mut metadata = UpdateMetadata {
        from: Some(Artifact::bundle(&args.from)),
        ..Default::default()
    };
    let mut screenshots = Vec::new();
    let mut app_ref = None;
    let outcome: anyhow::Result<()> = (|| {
        let prepare_bytes = fs::read(&args.prepare_recipe).context("reading preparation recipe")?;
        let verify_bytes = fs::read(&args.verify_recipe).context("reading verification recipe")?;
        let prepare = Recipe::parse(&prepare_bytes)?;
        let verify = Recipe::parse(&verify_bytes)?;
        validate_recipes(&prepare, &verify)?;
        let old = args.from.canonicalize().context("opening old bundle")?;
        let candidate = args
            .bundle
            .canonicalize()
            .context("opening candidate bundle")?;
        super::check_dependencies(deadline)
            .map_err(|error| failure(FailureReason::DependencyFailed, error.to_string()))?;
        ensure!(
            crate::tools::missing_tools(&["ostree"]).is_empty(),
            "verify-update-bundle requires ostree"
        );
        let mut workspace = Workspace::prepare()?;
        isolate_system_installations(
            &workspace.home,
            &mut workspace.env,
            &layout.runner_log,
            deadline,
        )?;
        let workspace = Rc::new(workspace);
        layout.append_runner_log(format!("update workspace: {}", workspace.home.display()))?;
        let runner = workspace.installer_runner(&layout);
        let installer = ArtifactInstaller::new(&runner, deadline);
        let before = installer.inspect_bundle(&old, &workspace.home.join("old-bundle"))?;
        let after =
            installer.inspect_bundle(&candidate, &workspace.home.join("candidate-bundle"))?;
        validate_refs(&before, &after)?;
        let app = AppRef::parse(&before.reference)?;
        app_ref = Some(app.to_string());
        stage = "install_old";
        installer.add_dependency_repo(args.dependency_repo.as_deref())?;
        installer.add_flathub_if_allowed(args.allow_network_remotes)?;
        installer
            .install_bundle(&old)
            .map_err(|e| failure(e.reason, e.message))?;
        metadata.before = installed(&installer, &before)?;
        stage = "prepare";
        let prepared = workspace.prepare_app_data(
            &app,
            &prepare,
            &args.prepare_recipe,
            &prepare_bytes,
            deadline,
        )?;
        metadata.before.recipe = Some(recipe_metadata(&prepare_bytes, Some(prepared.input_digest)));
        let prepare_layout = OutputLayout::prepare(&args.output.join("phases/prepare"), false)?;
        let mut desktop = start(&workspace, &app, &prepare_layout, &args, deadline)?;
        let selected = execute(
            &mut desktop,
            &workspace,
            &app,
            &prepare,
            &prepared.launch_args,
            &prepare_layout,
            &args,
            deadline,
            &mut screenshots,
            "prepare",
        )?;
        stage = "stop_old";
        let quit = prepare.quit.as_ref().expect("validated preparation quit");
        desktop
            .quit(selected, &quit.keys()?, bounded(deadline, quit.duration()?))
            .map_err(|error| {
                if error.downcast_ref::<super::CaptureFailure>().is_some() {
                    error
                } else {
                    failure(FailureReason::ShutdownFailed, format!("{error:#}"))
                }
            })?;
        drop(desktop);
        stage = "update";
        installer
            .update_bundle(&candidate)
            .map_err(|e| failure(e.reason, e.message))?;
        metadata.after = installed(&installer, &after)?;
        stage = "verify";
        // Only expand arguments here: never copy fixtures over the persisted profile.
        let profile = workspace.home.join(".var/app").join(app.id());
        let launch_args = verify
            .launch
            .args
            .iter()
            .map(|arg| super::setup::expand(arg, &profile))
            .collect::<anyhow::Result<Vec<_>>>()?;
        metadata.after.recipe = Some(recipe_metadata(&verify_bytes, None));
        let verify_layout = OutputLayout::prepare(&args.output.join("phases/verify"), false)?;
        let mut desktop = start(&workspace, &app, &verify_layout, &args, deadline)?;
        execute(
            &mut desktop,
            &workspace,
            &app,
            &verify,
            &launch_args,
            &verify_layout,
            &args,
            deadline,
            &mut screenshots,
            "verify",
        )?;
        Ok(())
    })();
    let timings = Timings {
        total: started.elapsed().as_millis(),
        ..Default::default()
    };
    let mut result = match &outcome {
        Ok(()) => RunResult::passed(
            app_ref.unwrap_or_default(),
            Artifact::bundle(&args.bundle),
            timings,
            screenshots,
        ),
        Err(error) => {
            metadata.failed_phase = Some(stage.into());
            RunResult::failed(
                app_ref,
                Artifact::bundle(&args.bundle),
                timings,
                screenshots,
                error
                    .downcast_ref::<super::CaptureFailure>()
                    .map(|e| e.reason)
                    .unwrap_or(FailureReason::InternalError),
                format!("{stage}: {error:#}"),
            )
        }
    };
    if let (Some(failed), Err(error)) = (&mut result.failure, &outcome) {
        failed.category = error
            .downcast_ref::<super::CaptureFailure>()
            .and_then(|e| e.category);
    }
    result.phase_metadata = metadata.after.clone();
    result.update = Some(metadata);
    layout.write_result(&result)?;
    outcome
}

fn validate_recipes(prepare: &Recipe, verify: &Recipe) -> anyhow::Result<()> {
    prepare.validate_setup()?;
    verify.validate_setup()?;
    ensure!(
        prepare.quit.is_some(),
        "preparation recipe requires quit: {{key: Ctrl+q, timeout: 10s}}"
    );
    ensure!(
        verify.quit.is_none(),
        "verification recipe must leave the app running for final health checks"
    );
    ensure!(
        verify.setup.files.is_empty(),
        "verification recipe cannot seed files into the existing profile"
    );
    ensure!(
        verify
            .steps
            .iter()
            .any(|step| matches!(step.action, Action::WaitText(_))),
        "verification recipe requires a wait_text persistence assertion"
    );
    Ok(())
}

fn validate_refs(before: &InstalledRef, after: &InstalledRef) -> anyhow::Result<()> {
    AppRef::parse(&before.reference)?;
    AppRef::parse(&after.reference)?;
    ensure!(
        before.reference == after.reference,
        "old and candidate must have the same app ID, architecture and branch"
    );
    ensure!(
        before.commit.is_some() && after.commit.is_some(),
        "bundle commits must be known"
    );
    ensure!(
        before.commit != after.commit,
        "candidate is the same commit as the old bundle"
    );
    Ok(())
}

fn installed(
    installer: &ArtifactInstaller<'_>,
    expected: &InstalledRef,
) -> anyhow::Result<PhaseMetadata> {
    let (app, runtimes) = installer.phase_refs(&expected.reference);
    ensure!(
        &app == expected,
        "installed deployment does not match the requested bundle commit"
    );
    Ok(PhaseMetadata {
        app: Some(app),
        runtimes,
        recipe: None,
    })
}

fn recipe_metadata(bytes: &[u8], input_digest: Option<String>) -> RecipeMetadata {
    RecipeMetadata {
        content_hash: format!(
            "sha256:{}",
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
        input_digest,
    }
}

fn start(
    workspace: &Rc<Workspace>,
    app: &AppRef,
    layout: &OutputLayout,
    args: &UpdateBundleArgs,
    deadline: Instant,
) -> anyhow::Result<Gnome> {
    let permissions = workspace
        .command("flatpak")
        .args(["info", "--user", "--show-permissions", &app.to_string()])
        .output_before_checked(deadline, &HealthCheck::default())?;
    ensure!(
        permissions.status.success(),
        "reading app permissions: {}",
        String::from_utf8_lossy(&permissions.stderr)
    );
    let backend = AppBackend::from_permissions(&String::from_utf8_lossy(&permissions.stdout));
    Gnome::start(
        workspace.clone(),
        layout,
        backend,
        bounded(deadline, args.display_timeout),
    )
}

#[allow(clippy::too_many_arguments)]
fn execute(
    desktop: &mut Gnome,
    workspace: &Workspace,
    app: &AppRef,
    recipe: &Recipe,
    launch_args: &[String],
    layout: &OutputLayout,
    args: &UpdateBundleArgs,
    deadline: Instant,
    screenshots: &mut Vec<String>,
    phase: &str,
) -> anyhow::Result<u64> {
    let mut manifest = Manifest::new();
    manifest.app_ref = Some(app.to_string());
    manifest.desktop_version = Some(desktop.version().to_owned());
    let outcome = (|| {
        desktop.launch_profile(
            app,
            launch_args,
            layout,
            bounded(deadline, args.window_timeout),
            true,
        )?;
        let workflow = Workflow {
            desktop,
            images: Images {
                workspace,
                logs: &layout.logs_dir,
                health: desktop.health(),
            },
            layout,
            overall: deadline,
        };
        let selected = workflow.initialize(recipe, bounded(deadline, args.window_timeout))?;
        workflow.execute(recipe, selected, args.screenshot_timeout, &mut manifest)
    })();
    screenshots.extend(
        manifest
            .captures
            .iter()
            .map(|capture| format!("phases/{phase}/{}", capture.path)),
    );
    if outcome.is_err() && layout.logs_dir.join("last-candidate.png").is_file() {
        screenshots.push(format!("phases/{phase}/logs/last-candidate.png"));
    }
    manifest.write(
        &args
            .output
            .join("phases")
            .join(phase)
            .join("screenshots.json"),
    )?;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_requires_matching_distinct_deployments() {
        let old = InstalledRef {
            reference: "app/org.example.Test/x86_64/stable".into(),
            commit: Some("a".into()),
        };
        let mut new = old.clone();
        assert!(validate_refs(&old, &new).is_err());
        new.commit = Some("b".into());
        assert!(validate_refs(&old, &new).is_ok());
        new.reference = "app/org.example.Test/x86_64/beta".into();
        assert!(validate_refs(&old, &new).is_err());
    }

    #[test]
    fn update_cannot_silently_reseed_or_skip_persistence_assertions() {
        let prepare = Recipe::parse(b"version: 1\nquit: {key: Ctrl+q, timeout: 10s}\nsteps: [{capture: {name: old, caption: Old}}]").unwrap();
        let mut verify = Recipe::parse(b"version: 1\nsteps: [{wait_text: Saved content}, {capture: {name: new, caption: New}}]").unwrap();
        assert!(validate_recipes(&prepare, &verify).is_ok());
        verify.setup.files.push(super::super::recipe::SeedFile {
            source: "seed".into(),
            destination: "data/state".into(),
        });
        assert!(validate_recipes(&prepare, &verify).is_err());
        verify.setup.files.clear();
        verify.steps.remove(0);
        assert!(validate_recipes(&prepare, &verify).is_err());
    }
}
