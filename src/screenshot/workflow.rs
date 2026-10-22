use super::{
    categorized_failure,
    desktop::{Desktop, Input, Window},
    failure,
    images::Images,
    infrastructure,
    manifest::{Capture, Manifest},
    recipe::{Action, Recipe, Step},
    snapshot::{self, CaptureSnapshot},
};
use crate::{
    analysis::FrameObservation,
    output::OutputLayout,
    process::{remaining, sleep_before_checked},
    result::{FailureCategory, FailureReason},
};
use anyhow::{Context, ensure};
use std::{
    fs,
    time::{Duration, Instant},
};

pub fn bounded(overall: Instant, requested: Duration) -> Instant {
    Instant::now()
        .checked_add(requested)
        .unwrap_or(overall)
        .min(overall)
}

pub struct Workflow<'a> {
    pub desktop: &'a dyn Desktop,
    pub images: Images<'a>,
    pub layout: &'a OutputLayout,
    pub overall: Instant,
}

impl Workflow<'_> {
    pub fn initialize(&self, recipe: &Recipe, deadline: Instant) -> anyhow::Result<u64> {
        let result = self.initialize_window(recipe, deadline);
        finish_check(result, self.desktop.check(deadline)).map_err(|error| {
            if Instant::now() >= deadline && error.downcast_ref::<super::CaptureFailure>().is_none()
            {
                failure(
                    FailureReason::WindowTimeout,
                    format!("initial window did not become ready: {error:#}"),
                )
            } else {
                error
            }
        })
    }

    fn initialize_window(&self, recipe: &Recipe, deadline: Instant) -> anyhow::Result<u64> {
        let window = self.select(recipe.window.title.as_deref(), deadline)?;
        let requested = &recipe.window;
        self.desktop
            .resize(window.id, requested.width, requested.height, deadline)?;
        loop {
            let current = self
                .desktop
                .windows(deadline)?
                .into_iter()
                .find(|current| current.id == window.id)
                .context("initial window closed while resizing")?;
            if current.frame.width == requested.width && current.frame.height == requested.height {
                break;
            }
            if sleep_before_checked(Duration::from_millis(100), deadline, &self.desktop.health())
                .is_err()
            {
                return Err(failure(
                    FailureReason::ScreenshotFailed,
                    format!(
                        "window did not accept {}x{}; actual size {}x{}",
                        requested.width,
                        requested.height,
                        current.frame.width,
                        current.frame.height
                    ),
                ));
            }
        }
        let snapshot = self.observe(window.id, deadline).map_err(|error| {
            if Instant::now() >= deadline && error.downcast_ref::<super::CaptureFailure>().is_none()
            {
                failure(
                    FailureReason::WindowTimeout,
                    format!("window did not become ready: {error:#}"),
                )
            } else {
                error
            }
        })?;
        snapshot.close()?;
        Ok(window.id)
    }

    pub fn execute(
        &self,
        recipe: &Recipe,
        mut selected: u64,
        default_timeout: Duration,
        manifest: &mut Manifest,
    ) -> anyhow::Result<()> {
        for (index, step) in recipe.steps.iter().enumerate() {
            manifest.failed_step_index = Some(index);
            self.layout
                .append_runner_log(format!("recipe step {index}"))?;
            let deadline = bounded(self.overall, step.timeout.unwrap_or(default_timeout));
            let result = self
                .step(step, &mut selected, deadline, manifest)
                .with_context(|| format!("recipe step {index}"));
            finish_check(result, self.desktop.check(deadline)).map_err(|error| {
                if error.downcast_ref::<super::CaptureFailure>().is_some() {
                    error
                } else {
                    failure(FailureReason::ScreenshotFailed, format!("{error:#}"))
                }
            })?;
        }
        manifest.failed_step_index = None;
        remaining(self.overall)?;
        self.desktop
            .windows(bounded(self.overall, Duration::from_secs(1)))
            .context("final helper check")
            .map_err(infrastructure)?;
        self.desktop.check(self.overall)
    }

    fn step(
        &self,
        step: &Step,
        selected: &mut u64,
        deadline: Instant,
        manifest: &mut Manifest,
    ) -> anyhow::Result<()> {
        match &step.action {
            Action::SelectWindow(selection) => {
                *selected = self.select(Some(&selection.title), deadline)?.id;
                let snapshot = self.observe(*selected, deadline)?;
                self.with_snapshot(snapshot, |_| Ok(()))?;
            }
            Action::Capture(capture) => {
                let snapshot = self.observe(*selected, deadline)?;
                self.with_snapshot(snapshot, |snapshot| {
                    let filename = format!("{:03}-{}.png", manifest.captures.len(), capture.name);
                    let path = self.layout.screenshot_path(&filename);
                    fs::copy(snapshot.path(), &path)
                        .context("publishing recipe screenshot")
                        .map_err(infrastructure)?;
                    manifest.captures.push(Capture {
                        name: capture.name.clone(),
                        path: self.layout.relative_screenshot_path(&filename),
                        caption: capture.caption.clone(),
                        language: "en".into(),
                        width: snapshot.image().width,
                        height: snapshot.image().height,
                        window_width: snapshot.window().frame.width,
                        window_height: snapshot.window().frame.height,
                    });
                    Ok(())
                })?;
            }
            Action::ClickText(label) => {
                let snapshot = self.snapshot(*selected, deadline)?;
                self.with_snapshot(snapshot, |snapshot| {
                    let tsv = self.images.ocr(snapshot.path(), deadline)?;
                    let matches = self.images.matches(&tsv, label);
                    self.layout
                        .append_runner_log(format!("click candidates: {matches:?}"))?;
                    if matches.len() != 1 {
                        return Err(categorized_failure(
                            FailureReason::ScreenshotFailed,
                            FailureCategory::Expectation,
                            format!(
                                "click text '{label}' matched {} locations; expected exactly one",
                                matches.len()
                            ),
                        ));
                    }
                    let (x, y) = matches[0];
                    let window = snapshot.window();
                    ensure!(
                        x >= 0.0
                            && y >= 0.0
                            && x < f64::from(window.buffer.width)
                            && y < f64::from(window.buffer.height),
                        "click target is outside the selected window"
                    );
                    self.desktop.input(
                        *selected,
                        Input::Click(x / window.scale, y / window.scale),
                        deadline,
                    )?;
                    Ok(())
                })?;
            }
            Action::TypeText(text) => {
                self.desktop.input(*selected, Input::Text(text), deadline)?;
            }
            Action::Key(keys) => {
                self.desktop.input(*selected, Input::Key(keys), deadline)?;
            }
            Action::WaitText(label) => loop {
                let snapshot = self.snapshot(*selected, deadline)?;
                let found = self.with_snapshot(snapshot, |snapshot| {
                    let tsv = self.images.ocr(snapshot.path(), deadline)?;
                    Ok(!self.images.matches(&tsv, label).is_empty())
                })?;
                if found {
                    break;
                }
                sleep_before_checked(Duration::from_millis(150), deadline, &self.desktop.health())
                    .with_context(|| format!("waiting for text '{label}'"))?;
            },
        }
        Ok(())
    }

    fn select(&self, title: Option<&str>, deadline: Instant) -> anyhow::Result<Window> {
        loop {
            let windows: Vec<_> = self
                .desktop
                .windows(deadline)?
                .into_iter()
                .filter(|window| title.map_or(!window.transient, |title| window.title == title))
                .collect();
            ensure!(
                windows.len() <= 1,
                "ambiguous window selection: {} matching windows; specify a unique title",
                windows.len()
            );
            if let Some(window) = windows.into_iter().next() {
                return Ok(window);
            }
            sleep_before_checked(Duration::from_millis(100), deadline, &self.desktop.health())
                .map_err(|error| {
                    if error.kind() != std::io::ErrorKind::TimedOut {
                        return error.into();
                    }
                    categorized_failure(
                        FailureReason::WindowTimeout,
                        FailureCategory::Expectation,
                        format!("waiting for app window {title:?}: {error}"),
                    )
                })?;
        }
    }

    fn snapshot(&self, id: u64, deadline: Instant) -> anyhow::Result<CaptureSnapshot> {
        snapshot::capture(
            self.desktop,
            &self.images,
            &self.layout.logs_dir,
            id,
            deadline,
        )
    }

    fn observe(&self, id: u64, deadline: Instant) -> anyhow::Result<CaptureSnapshot> {
        let mut observation = FrameObservation::default();
        loop {
            let snapshot = self.snapshot(id, deadline)?;
            if observation.observe(snapshot.image().has_content, Instant::now()) {
                return Ok(snapshot);
            }
            snapshot.close()?;
            sleep_before_checked(Duration::from_millis(150), deadline, &self.desktop.health())?;
        }
    }

    fn with_snapshot<T>(
        &self,
        snapshot: CaptureSnapshot,
        use_snapshot: impl FnOnce(&CaptureSnapshot) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let result = use_snapshot(&snapshot);
        let cleanup = snapshot.close();
        finish_snapshot(result, cleanup)
    }
}

// Concrete liveness failures take precedence. A bare deadline check must not
// erase the more informative failure that caused a step to stop.
fn finish_check<T>(result: anyhow::Result<T>, check: anyhow::Result<()>) -> anyhow::Result<T> {
    match check {
        Err(error) if error.downcast_ref::<super::CaptureFailure>().is_some() => Err(error),
        check => result.and_then(|value| check.map(|()| value)),
    }
}

fn finish_snapshot<T>(
    use_result: anyhow::Result<T>,
    cleanup_result: anyhow::Result<()>,
) -> anyhow::Result<T> {
    match (use_result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => Err(cleanup).context("cleaning screenshot snapshot"),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(error.context(format!(
            "also failed to clean screenshot snapshot: {cleanup:#}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Workflow, finish_snapshot};
    use crate::{
        process::HealthCheck,
        screenshot::{
            desktop::{Desktop, Input, Rect, Window},
            images::{ImageInfo, Images},
            snapshot::capture_with_inspector,
            workspace::Workspace,
        },
    };
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    struct FakeDesktop(
        Window,
        Option<std::path::PathBuf>,
        Option<fn() -> anyhow::Error>,
    );

    impl Desktop for FakeDesktop {
        fn version(&self) -> &str {
            "test"
        }
        fn health(&self) -> HealthCheck {
            HealthCheck::default()
        }
        fn check(&self, _deadline: Instant) -> anyhow::Result<()> {
            Ok(())
        }
        fn windows(&self, _deadline: Instant) -> anyhow::Result<Vec<Window>> {
            if let Some(error) = self.2 {
                return Err(error());
            }
            Ok(vec![self.0.clone()])
        }
        fn resize(
            &self,
            _window: u64,
            _width: u32,
            _height: u32,
            _deadline: Instant,
        ) -> anyhow::Result<Window> {
            Ok(self.0.clone())
        }
        fn capture(&self, _window: u64, path: &Path, _deadline: Instant) -> anyhow::Result<Window> {
            if let Some(fixture) = &self.1 {
                fs::copy(fixture, path)?;
            } else {
                fs::write(path, b"snapshot bytes")?;
            }
            Ok(self.0.clone())
        }
        fn input(&self, _window: u64, _input: Input<'_>, _deadline: Instant) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn window() -> Window {
        Window {
            id: 9,
            title: "Main".into(),
            app_id: "org.example.Test".into(),
            transient: false,
            frame: Rect {
                x: 0,
                y: 0,
                width: 900,
                height: 600,
            },
            buffer: Rect {
                x: 0,
                y: 0,
                width: 900,
                height: 600,
            },
            scale: 1.0,
        }
    }

    fn workflow<'a>(
        desktop: &'a FakeDesktop,
        workspace: &'a Workspace,
        layout: &'a crate::output::OutputLayout,
    ) -> Workflow<'a> {
        Workflow {
            desktop,
            images: Images {
                workspace,
                logs: &layout.logs_dir,
                health: desktop.health(),
            },
            layout,
            overall: Instant::now() + Duration::from_secs(5),
        }
    }

    #[test]
    #[ignore = "requires ImageMagick"]
    fn recipe_publication_checks_real_pixels() {
        use crate::result::FailureReason;
        use crate::screenshot::{manifest::Manifest, recipe::Recipe};

        // These are window pixels after GNOME excludes the compositor cursor.
        // A cursor drawn by the app itself is intentionally not shape-classified.
        for (drawing, expected) in [
            ("", false),
            ("fill white rectangle 0,0 899,599", false),
            ("fill gray rectangle 0,0 899,63", false),
            ("fill white rectangle 200,250 219,269", true),
            (
                "fill #242424 rectangle 0,0 899,599 fill white rectangle 200,250 400,270",
                true,
            ),
            (
                "fill white polygon 450,300 450,320 456,314 462,325 466,323 460,312 470,312",
                true,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let fixture = root.path().join("fixture.png");
            let mut convert = std::process::Command::new("convert");
            convert.args(["-size", "900x600", "xc:black"]);
            if !drawing.is_empty() {
                convert.args(["-draw", drawing]);
            }
            assert!(convert.arg(&fixture).status().unwrap().success());
            let layout =
                crate::output::OutputLayout::prepare(&root.path().join("output"), false).unwrap();
            let desktop = FakeDesktop(window(), Some(fixture.clone()), None);
            let workspace = Workspace::prepare().unwrap();
            let workflow = workflow(&desktop, &workspace, &layout);
            let recipe: Recipe = serde_yaml_ng::from_str(
                "version: 1\nsteps:\n  - capture:\n      name: main\n      caption: Main view\n",
            )
            .unwrap();
            let mut manifest = Manifest::new();
            let result = workflow.execute(&recipe, 9, Duration::from_secs(2), &mut manifest);
            assert_eq!(result.is_ok(), expected, "{drawing}: {result:?}");
            let published = layout.screenshot_path("000-main.png");
            assert_eq!(published.exists(), expected);
            assert_eq!(manifest.captures.len(), usize::from(expected));
            assert_eq!(
                fs::read(layout.logs_dir.join("last-candidate.png")).unwrap(),
                fs::read(&fixture).unwrap(),
            );
            if expected {
                assert_eq!(fs::read(published).unwrap(), fs::read(&fixture).unwrap());
                assert_eq!(manifest.failed_step_index, None);
            } else {
                assert_eq!(manifest.failed_step_index, Some(0));
                assert_eq!(
                    result
                        .unwrap_err()
                        .downcast_ref::<crate::screenshot::CaptureFailure>()
                        .unwrap()
                        .reason,
                    FailureReason::ScreenshotFailed
                );
                let deadline = Instant::now() + Duration::from_millis(500);
                assert_eq!(
                    workflow
                        .initialize(&recipe, deadline)
                        .unwrap_err()
                        .downcast_ref::<crate::screenshot::CaptureFailure>()
                        .unwrap()
                        .reason,
                    FailureReason::WindowTimeout
                );
            }
        }
    }

    #[test]
    fn health_check_does_not_erase_classification_with_bare_timeout() {
        use crate::{
            result::{FailureCategory, FailureReason},
            screenshot::{CaptureFailure, categorized_failure},
        };
        for category in [
            FailureCategory::Infrastructure,
            FailureCategory::Expectation,
        ] {
            let primary =
                categorized_failure(FailureReason::ScreenshotFailed, category, "primary failure");
            let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline elapsed");
            let error = super::finish_check::<()>(Err(primary), Err(timeout.into())).unwrap_err();
            assert_eq!(
                error.downcast_ref::<CaptureFailure>().unwrap().category,
                Some(category)
            );
        }
        let primary = categorized_failure(
            FailureReason::WindowTimeout,
            FailureCategory::Expectation,
            "missing target",
        );
        let health = categorized_failure(
            FailureReason::ScreenshotFailed,
            FailureCategory::Infrastructure,
            "helper disconnected",
        );
        let error = super::finish_check::<()>(Err(primary), Err(health)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<CaptureFailure>().unwrap().category,
            Some(FailureCategory::Infrastructure)
        );
    }

    #[test]
    fn missing_recipe_window_is_an_expectation_failure() {
        use crate::{
            result::{FailureCategory, FailureReason},
            screenshot::{CaptureFailure, manifest::Manifest, recipe::Recipe},
        };
        let root = tempfile::tempdir().unwrap();
        let layout = crate::output::OutputLayout::prepare(root.path(), false).unwrap();
        let desktop = FakeDesktop(window(), None, None);
        let workspace = Workspace::prepare().unwrap();
        let workflow = workflow(&desktop, &workspace, &layout);
        let recipe: Recipe = serde_yaml_ng::from_str(
            "version: 1\nsteps:\n  - select_window:\n      title: Missing\n",
        )
        .unwrap();
        let mut manifest = Manifest::new();
        let error = workflow
            .execute(&recipe, 9, Duration::from_millis(5), &mut manifest)
            .unwrap_err();
        let failure = error.downcast_ref::<CaptureFailure>().unwrap();
        assert_eq!(failure.reason, FailureReason::WindowTimeout);
        assert_eq!(failure.category, Some(FailureCategory::Expectation));
        assert_eq!(manifest.failed_step_index, Some(0));
    }

    #[test]
    fn final_helper_check_preserves_specific_reason() {
        use crate::{
            result::FailureReason,
            screenshot::{CaptureFailure, manifest::Manifest, recipe::Recipe},
        };
        let root = tempfile::tempdir().unwrap();
        let layout = crate::output::OutputLayout::prepare(root.path(), false).unwrap();
        let desktop = FakeDesktop(
            window(),
            None,
            Some(|| crate::screenshot::failure(FailureReason::DisplayExited, "desktop failed")),
        );
        let workspace = Workspace::prepare().unwrap();
        let workflow = workflow(&desktop, &workspace, &layout);
        let recipe: Recipe =
            serde_yaml_ng::from_str("version: 1\nsteps:\n  - type_text: hello\n").unwrap();
        let mut manifest = Manifest::new();
        let error = workflow
            .execute(&recipe, 9, Duration::from_secs(1), &mut manifest)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<CaptureFailure>().unwrap().reason,
            FailureReason::DisplayExited
        );
        assert!(format!("{error:#}").contains("final helper check"));
        assert_eq!(manifest.failed_step_index, None);
    }

    #[test]
    fn helper_transport_failure_is_infrastructure_during_step_and_final_check() {
        use crate::{
            result::{FailureCategory, FailureReason},
            screenshot::{CaptureFailure, manifest::Manifest, recipe::Recipe},
        };
        let root = tempfile::tempdir().unwrap();
        let layout = crate::output::OutputLayout::prepare(root.path(), false).unwrap();
        let workspace = Workspace::prepare().unwrap();
        for (step, failed_index) in [
            ("select_window:\n      title: Main", Some(0)),
            ("type_text: hello", None),
        ] {
            let desktop = FakeDesktop(
                window(),
                None,
                Some(|| {
                    crate::screenshot::infrastructure(anyhow::anyhow!(
                        "helper transport disconnected"
                    ))
                }),
            );
            let workflow = workflow(&desktop, &workspace, &layout);
            let recipe: Recipe =
                serde_yaml_ng::from_str(&format!("version: 1\nsteps:\n  - {step}\n")).unwrap();
            let mut manifest = Manifest::new();
            let error = workflow
                .execute(&recipe, 9, Duration::from_secs(1), &mut manifest)
                .unwrap_err();
            let failure = error.downcast_ref::<CaptureFailure>().unwrap();
            assert_eq!(failure.reason, FailureReason::ScreenshotFailed);
            assert_eq!(failure.category, Some(FailureCategory::Infrastructure));
            assert_eq!(manifest.failed_step_index, failed_index);
        }
    }

    #[test]
    #[ignore = "requires ImageMagick"]
    fn publication_output_copy_failure_is_infrastructure() {
        use crate::{
            result::{FailureCategory, FailureReason},
            screenshot::{CaptureFailure, manifest::Manifest, recipe::Recipe},
        };
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("fixture.png");
        assert!(
            std::process::Command::new("convert")
                .args([
                    "-size",
                    "900x600",
                    "xc:black",
                    "-fill",
                    "white",
                    "-draw",
                    "rectangle 200,250 400,270"
                ])
                .arg(&fixture)
                .status()
                .unwrap()
                .success()
        );
        let layout =
            crate::output::OutputLayout::prepare(&root.path().join("output"), false).unwrap();
        fs::create_dir(layout.screenshot_path("000-main.png")).unwrap();
        let desktop = FakeDesktop(window(), Some(fixture), None);
        let workspace = Workspace::prepare().unwrap();
        let workflow = workflow(&desktop, &workspace, &layout);
        let recipe: Recipe = serde_yaml_ng::from_str(
            "version: 1\nsteps:\n  - capture:\n      name: main\n      caption: Main view\n",
        )
        .unwrap();
        let mut manifest = Manifest::new();
        let error = workflow
            .execute(&recipe, 9, Duration::from_secs(2), &mut manifest)
            .unwrap_err();
        let failure = error.downcast_ref::<CaptureFailure>().unwrap();
        assert_eq!(failure.reason, FailureReason::ScreenshotFailed);
        assert_eq!(failure.category, Some(FailureCategory::Infrastructure));
        assert!(format!("{error:#}").contains("publishing recipe screenshot"));
        assert!(manifest.captures.is_empty());
        assert_eq!(manifest.failed_step_index, Some(0));
    }

    #[test]
    fn finish_snapshot_preserves_primary_error() {
        let result: anyhow::Result<()> = Err(anyhow::anyhow!("action failed"));
        let cleanup = Err(anyhow::anyhow!("cleanup failed"));

        let error = finish_snapshot(result, cleanup).unwrap_err();

        assert_eq!(error.root_cause().to_string(), "action failed");
        assert!(format!("{error:#}").contains("cleanup failed"));
    }

    #[test]
    fn finish_snapshot_reports_cleanup_error_after_success() {
        let error = finish_snapshot(Ok(42), Err(anyhow::anyhow!("cleanup failed"))).unwrap_err();
        assert!(format!("{error:#}").contains("cleanup failed"));
    }

    #[test]
    fn with_snapshot_passes_stable_path_to_consumer() {
        let root = tempfile::tempdir().unwrap();
        let layout = crate::output::OutputLayout::prepare(root.path(), false).unwrap();
        let desktop = FakeDesktop(window(), None, None);
        let workspace = Workspace::prepare().unwrap();
        let workflow = workflow(&desktop, &workspace, &layout);
        let snapshot = capture_with_inspector(
            &desktop,
            &layout.logs_dir,
            desktop.0.id,
            Instant::now() + Duration::from_secs(2),
            |_, _, _| {
                Ok(ImageInfo {
                    width: 900,
                    height: 600,
                    has_content: true,
                })
            },
        )
        .unwrap();

        let path = workflow
            .with_snapshot(snapshot, |snapshot| {
                assert!(snapshot.path().exists());
                assert_eq!(fs::read(snapshot.path())?, b"snapshot bytes");
                Ok(snapshot.path().to_path_buf())
            })
            .unwrap();

        assert!(!path.exists());
        assert_eq!(
            fs::read(layout.logs_dir.join("last-candidate.png")).unwrap(),
            b"snapshot bytes"
        );

        let failed_snapshot = capture_with_inspector(
            &desktop,
            &layout.logs_dir,
            desktop.0.id,
            Instant::now() + Duration::from_secs(2),
            |_, _, _| {
                Ok(ImageInfo {
                    width: 900,
                    height: 600,
                    has_content: true,
                })
            },
        )
        .unwrap();
        let failed_path = failed_snapshot.path().to_path_buf();
        let error = workflow
            .with_snapshot(failed_snapshot, |_| {
                Err::<(), _>(anyhow::anyhow!("OCR failed"))
            })
            .unwrap_err();

        assert!(format!("{error:#}").contains("OCR failed"));
        assert!(!failed_path.exists());
        assert_eq!(
            fs::read(layout.logs_dir.join("last-candidate.png")).unwrap(),
            b"snapshot bytes"
        );
    }
}
