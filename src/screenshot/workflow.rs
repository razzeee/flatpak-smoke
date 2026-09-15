use super::{
    desktop::{Desktop, Input, Window},
    failure,
    images::{ImageInfo, Images},
    manifest::{Capture, Manifest},
    recipe::{Action, Recipe, Step},
};
use crate::{
    output::OutputLayout,
    process::{remaining, sleep_before_checked},
    result::FailureReason,
    session::FrameObservation,
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
        self.desktop.check()?;
        result.map_err(|error| {
            if Instant::now() >= deadline {
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
        self.observe(window.id, deadline).map_err(|error| {
            if Instant::now() >= deadline {
                failure(
                    FailureReason::WindowTimeout,
                    format!("window did not become ready: {error:#}"),
                )
            } else {
                error
            }
        })?;
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
            self.desktop.check()?;
            result.map_err(|error| {
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
            .map_err(|error| {
                failure(
                    FailureReason::ScreenshotFailed,
                    format!("final helper check: {error:#}"),
                )
            })?;
        self.desktop.check()
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
                self.observe(*selected, deadline)?;
            }
            Action::Capture(capture) => {
                let (window, image) = self.observe(*selected, deadline)?;
                let filename = format!("{:03}-{}.png", manifest.captures.len(), capture.name);
                let path = self.layout.screenshot_path(&filename);
                fs::copy(self.images.candidate_path(), &path)?;
                manifest.captures.push(Capture {
                    name: capture.name.clone(),
                    path: self.layout.relative_screenshot_path(&filename),
                    caption: capture.caption.clone(),
                    language: "en".into(),
                    width: image.width,
                    height: image.height,
                    window_width: window.frame.width,
                    window_height: window.frame.height,
                });
            }
            Action::ClickText(label) => {
                let (window, _) = self.snapshot(*selected, deadline)?;
                let tsv = self.images.ocr(&self.images.candidate_path(), deadline)?;
                let matches = self.images.matches(&tsv, label);
                self.layout
                    .append_runner_log(format!("click candidates: {matches:?}"))?;
                ensure!(
                    matches.len() == 1,
                    "click text '{label}' matched {} locations; expected exactly one",
                    matches.len()
                );
                let (x, y) = matches[0];
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
            }
            Action::TypeText(text) => {
                self.desktop.input(*selected, Input::Text(text), deadline)?;
            }
            Action::Key(keys) => {
                self.desktop.input(*selected, Input::Key(keys), deadline)?;
            }
            Action::WaitText(label) => loop {
                self.snapshot(*selected, deadline)?;
                let tsv = self.images.ocr(&self.images.candidate_path(), deadline)?;
                if !self.images.matches(&tsv, label).is_empty() {
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
                    failure(
                        FailureReason::WindowTimeout,
                        format!("waiting for app window {title:?}: {error}"),
                    )
                })?;
        }
    }

    fn snapshot(&self, id: u64, deadline: Instant) -> anyhow::Result<(Window, ImageInfo)> {
        self.desktop.check()?;
        let pending = self.layout.logs_dir.join("pending.png");
        let window = self.desktop.capture(id, &pending, deadline)?;
        let image = self.images.inspect(&pending, &window, deadline)?;
        fs::rename(&pending, self.images.candidate_path())?;
        ensure!(
            window.scale == 1.0,
            "unsupported window scale {}; expected 1",
            window.scale
        );
        ensure!(
            window.frame.width <= 1000 && window.frame.height <= 700,
            "window size {}x{} exceeds 1000x700",
            window.frame.width,
            window.frame.height
        );
        ensure!(
            image.width == window.buffer.width && image.height == window.buffer.height,
            "native image and window buffer dimensions differ"
        );
        Ok((window, image))
    }

    fn observe(&self, id: u64, deadline: Instant) -> anyhow::Result<(Window, ImageInfo)> {
        let mut observation = FrameObservation::default();
        loop {
            let (window, image) = self.snapshot(id, deadline)?;
            if observation.observe(image.has_content, Instant::now()) {
                return Ok((window, image));
            }
            sleep_before_checked(Duration::from_millis(150), deadline, &self.desktop.health())?;
        }
    }
}
