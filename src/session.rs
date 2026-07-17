use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use crate::{command::ensure_file_nonempty, output::OutputLayout, result::FailureReason};

pub struct SessionRunner<'a> {
    layout: &'a OutputLayout,
    env: Vec<(OsString, OsString)>,
    display_timeout: Duration,
    window_timeout: Duration,
    screenshot_timeout: Duration,
    overall_deadline: Instant,
}

impl<'a> SessionRunner<'a> {
    pub fn new(
        layout: &'a OutputLayout,
        env: Vec<(OsString, OsString)>,
        display_timeout: Duration,
        window_timeout: Duration,
        screenshot_timeout: Duration,
        overall_deadline: Instant,
    ) -> Self {
        Self {
            layout,
            env,
            display_timeout,
            window_timeout,
            screenshot_timeout,
            overall_deadline,
        }
    }

    pub fn launch_wait_and_capture(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        screenshots_after_click: &[String],
    ) -> Result<SessionSuccess, SessionError> {
        let display_lease = DisplayLease::acquire()?;
        let display = display_lease.display();
        self.layout
            .append_runner_log(format!("starting Xvfb display {display}"))
            .map_err(SessionError::internal)?;

        let mut xvfb = self.spawn_xvfb(&display)?;
        let session_result = self.run_inside_display(
            app_ref,
            screenshot_name,
            screenshots_after_click,
            &display,
            &mut xvfb,
        );
        terminate_child(&mut xvfb);
        session_result
    }

    fn run_inside_display(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        screenshots_after_click: &[String],
        display: &str,
        xvfb: &mut Child,
    ) -> Result<SessionSuccess, SessionError> {
        self.wait_for_display(display, xvfb)?;
        let mut window_manager = self.spawn_openbox(display)?;
        let result =
            self.run_app_session(app_ref, screenshot_name, screenshots_after_click, display);
        terminate_child(&mut window_manager);
        result
    }

    fn run_app_session(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        screenshots_after_click: &[String],
        display: &str,
    ) -> Result<SessionSuccess, SessionError> {
        let launch_started = Instant::now();
        let mut app = self.spawn_app(app_ref, display)?;
        let result = (|| {
            let window_detector =
                WindowDetector::new(display, self.env.clone(), self.layout.runner_log.clone());
            let window = window_detector
                .wait_for_visible_window(&mut app, self.bounded_timeout(self.window_timeout)?)?;
            let launch_to_window = launch_started.elapsed().as_millis();

            if window.is_app_error_window() {
                return Err(SessionError::new(
                    FailureReason::AppErrorWindow,
                    format!(
                        "visible app error window detected (id {}, title '{}')",
                        window.id,
                        window.display_title()
                    ),
                ));
            }

            if let Some(status) = app.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::EarlyExit,
                    format!("app exited before screenshot with {status}"),
                ));
            }

            thread::sleep(Duration::from_millis(500));

            let mut screenshot_path = self.layout.screenshot_path(screenshot_name);
            let relative_screenshot_path = self.layout.relative_screenshot_path(screenshot_name);
            let screenshotter =
                Screenshotter::new(display, self.env.clone(), self.layout.runner_log.clone());
            let mut screenshots = Vec::new();
            screenshotter.capture(
                &screenshot_path,
                self.bounded_timeout(self.screenshot_timeout)?,
            )?;
            screenshots.push(relative_screenshot_path);
            if let Some(marker) = screenshotter
                .detect_app_error_text(&screenshot_path)
                .map_err(|error| error.with_screenshots(screenshots.clone()))?
            {
                return Err(SessionError::new(
                    FailureReason::AppErrorWindow,
                    format!("screenshot text matched app error marker '{marker}'"),
                )
                .with_screenshots(screenshots));
            }

            for (index, click_text) in screenshots_after_click.iter().enumerate() {
                let click_target = screenshotter
                    .find_button_click_target(&screenshot_path, click_text)
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?;
                screenshotter
                    .click(click_target)
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?;
                thread::sleep(Duration::from_millis(500));

                let next_name = click_screenshot_name(index + 1, click_text);
                let next_path = self.layout.screenshot_path(&next_name);
                let next_relative_path = self.layout.relative_screenshot_path(&next_name);
                screenshotter
                    .capture(&next_path, self.bounded_timeout(self.screenshot_timeout)?)
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?;
                screenshots.push(next_relative_path);
                if let Some(marker) = screenshotter
                    .detect_app_error_text(&next_path)
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?
                {
                    return Err(SessionError::new(
                        FailureReason::AppErrorWindow,
                        format!("screenshot text matched app error marker '{marker}'"),
                    )
                    .with_screenshots(screenshots));
                }

                screenshot_path = next_path;
            }

            Ok(SessionSuccess {
                screenshot_paths: screenshots,
                launch_to_window_ms: launch_to_window,
            })
        })();

        terminate_child(&mut app);
        terminate_keyring_unlock_daemons(&self.env);
        result
    }

    fn spawn_xvfb(&self, display: &str) -> Result<Child, SessionError> {
        let stdout = File::create(self.layout.logs_dir.join("xvfb.stdout.log"))
            .map_err(SessionError::internal)?;
        let stderr = File::create(self.layout.logs_dir.join("xvfb.stderr.log"))
            .map_err(SessionError::internal)?;
        std::process::Command::new("Xvfb")
            .arg(display)
            .args(["-screen", "0", "1280x720x24", "-nolisten", "tcp"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("failed to start Xvfb: {error}"),
                )
            })
    }

    fn wait_for_display(&self, display: &str, xvfb: &mut Child) -> Result<(), SessionError> {
        let timeout = self.bounded_timeout(self.display_timeout)?;
        let started = Instant::now();
        while started.elapsed() < timeout {
            if let Some(status) = xvfb.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("Xvfb exited before the display became ready with {status}"),
                ));
            }

            let status = self
                .base_command("xdotool", display)
                .arg("getdisplaygeometry")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            if status.map(|status| status.success()).unwrap_or(false) {
                return match xvfb.try_wait().map_err(SessionError::internal)? {
                    Some(status) => Err(SessionError::new(
                        FailureReason::DisplayStartFailed,
                        format!("Xvfb exited after display readiness check with {status}"),
                    )),
                    None => Ok(()),
                };
            }

            thread::sleep(Duration::from_millis(100));
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!(
                "Xvfb display did not become ready within {}s",
                timeout.as_secs()
            ),
        ))
    }

    fn bounded_timeout(&self, requested: Duration) -> Result<Duration, SessionError> {
        let remaining = self
            .overall_deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SessionError::new(
                FailureReason::InternalError,
                "overall timeout elapsed",
            ));
        }
        Ok(requested.min(remaining))
    }

    fn spawn_openbox(&self, display: &str) -> Result<Child, SessionError> {
        let stdout = File::create(self.layout.logs_dir.join("openbox.stdout.log"))
            .map_err(SessionError::internal)?;
        let stderr = File::create(self.layout.logs_dir.join("openbox.stderr.log"))
            .map_err(SessionError::internal)?;
        self.base_command("openbox", display)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("failed to start openbox: {error}"),
                )
            })
    }

    fn spawn_app(&self, app_ref: &str, display: &str) -> Result<Child, SessionError> {
        let stdout = File::create(&self.layout.app_stdout).map_err(SessionError::internal)?;
        let stderr = File::create(&self.layout.app_stderr).map_err(SessionError::internal)?;
        let run_target = flatpak_run_target(app_ref);

        self.base_command("dbus-run-session", display)
            .args([
                "--",
                "sh",
                "-c",
                START_DESKTOP_SERVICES_AND_RUN_FLATPAK,
                "flatpak-smoke-session",
            ])
            .args([
                "--env=GDK_BACKEND=x11",
                "--env=GSK_RENDERER=cairo",
                "--env=GTK_A11Y=none",
                "--env=LIBGL_ALWAYS_SOFTWARE=1",
            ])
            .arg(&run_target)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                SessionError::new(
                    FailureReason::LaunchFailed,
                    format!("failed to launch app '{run_target}': {error}"),
                )
            })
    }

    fn base_command(&self, program: &str, display: &str) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.env("DISPLAY", display);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
}

const START_DESKTOP_SERVICES_AND_RUN_FLATPAK: &str = r#"
set -eu
printf '\n' | gnome-keyring-daemon --unlock --components=secrets >/dev/null
exec flatpak run "$@"
"#;

struct DisplayLease {
    number: u16,
    lock_path: PathBuf,
}

impl DisplayLease {
    fn acquire() -> Result<Self, SessionError> {
        Self::acquire_in(&env::temp_dir().join("flatpak-smoke-displays"), 100..500)
    }

    fn acquire_in(root: &Path, displays: std::ops::Range<u16>) -> Result<Self, SessionError> {
        fs::create_dir_all(root).map_err(SessionError::internal)?;

        for number in displays {
            if !Self::display_appears_free(number) {
                continue;
            }

            let lock_path = root.join(format!("display-{number}.lock"));
            Self::remove_stale_lock(&lock_path);

            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut lock_file) => {
                    writeln!(lock_file, "{}", std::process::id())
                        .map_err(SessionError::internal)?;

                    if Self::display_appears_free(number) {
                        return Ok(Self { number, lock_path });
                    }

                    let _ = fs::remove_file(&lock_path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(SessionError::internal(error)),
            }
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            "no free X display found in range :100-:499",
        ))
    }

    fn display(&self) -> String {
        format!(":{}", self.number)
    }

    fn display_appears_free(number: u16) -> bool {
        !Path::new(&format!("/tmp/.X{number}-lock")).exists()
            && !Path::new("/tmp/.X11-unix")
                .join(format!("X{number}"))
                .exists()
    }

    fn remove_stale_lock(lock_path: &Path) {
        let Ok(contents) = fs::read_to_string(lock_path) else {
            return;
        };
        let Ok(pid) = contents.trim().parse::<u32>() else {
            return;
        };
        if !Path::new("/proc").join(pid.to_string()).exists() {
            let _ = fs::remove_file(lock_path);
        }
    }
}

impl Drop for DisplayLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

#[derive(Debug, Clone)]
pub struct SessionSuccess {
    pub screenshot_paths: Vec<String>,
    pub launch_to_window_ms: u128,
}

#[derive(Debug, Clone)]
pub struct SessionError {
    pub reason: FailureReason,
    pub message: String,
    pub screenshots: Vec<String>,
}

impl SessionError {
    fn new(reason: FailureReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            screenshots: Vec::new(),
        }
    }

    fn with_screenshots(mut self, screenshot_paths: Vec<String>) -> Self {
        self.screenshots = screenshot_paths;
        self
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(FailureReason::InternalError, error.to_string())
    }
}

struct WindowDetector {
    display: String,
    env: Vec<(OsString, OsString)>,
    runner_log: PathBuf,
}

impl WindowDetector {
    fn new(display: &str, env: Vec<(OsString, OsString)>, runner_log: PathBuf) -> Self {
        Self {
            display: display.to_string(),
            env,
            runner_log,
        }
    }

    fn wait_for_visible_window(
        &self,
        app: &mut Child,
        timeout: Duration,
    ) -> Result<VisibleWindow, SessionError> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if let Some(status) = app.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::EarlyExit,
                    format!("app exited before a visible window appeared with {status}"),
                ));
            }

            let output = self
                .command("xdotool")
                .args(["search", "--onlyvisible", "."])
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    if let Some(window_id) = visible_window_ids(&output.stdout).into_iter().next() {
                        let window = self.describe_window(window_id)?;
                        self.append_log(format!(
                            "visible window detected: id={} title='{}'",
                            window.id,
                            window.display_title()
                        ))?;
                        return Ok(window);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(SessionError::new(
                        FailureReason::DisplayStartFailed,
                        format!("xdotool failed during window detection: {error}"),
                    ));
                }
            }

            thread::sleep(Duration::from_millis(200));
        }

        Err(SessionError::new(
            FailureReason::WindowTimeout,
            format!("no visible window appeared within {}s", timeout.as_secs()),
        ))
    }

    fn describe_window(&self, id: String) -> Result<VisibleWindow, SessionError> {
        let output = self
            .command("xdotool")
            .args(["getwindowname", &id])
            .output();

        match output {
            Ok(output) if output.status.success() => Ok(VisibleWindow {
                id,
                title: trimmed_nonempty(&output.stdout),
            }),
            Ok(output) => {
                self.append_log(format!(
                    "failed to read visible window title for id={}: {}",
                    id,
                    String::from_utf8_lossy(&output.stderr).trim()
                ))?;
                Ok(VisibleWindow { id, title: None })
            }
            Err(error) => {
                self.append_log(format!(
                    "failed to run xdotool getwindowname for id={id}: {error}"
                ))?;
                Ok(VisibleWindow { id, title: None })
            }
        }
    }

    fn command(&self, program: &str) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.env("DISPLAY", &self.display);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    fn append_log(&self, message: impl AsRef<str>) -> Result<(), SessionError> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.runner_log)
            .and_then(|mut file| writeln!(file, "{}", message.as_ref()))
            .map_err(SessionError::internal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisibleWindow {
    id: String,
    title: Option<String>,
}

impl VisibleWindow {
    fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("<unknown>")
    }

    fn is_app_error_window(&self) -> bool {
        self.title.as_deref().is_some_and(is_app_error_window_title)
    }
}

fn visible_window_ids(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn trimmed_nonempty(stdout: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn is_app_error_window_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case("error")
}

struct Screenshotter {
    display: String,
    env: Vec<(OsString, OsString)>,
    runner_log: PathBuf,
}

impl Screenshotter {
    fn new(display: &str, env: Vec<(OsString, OsString)>, runner_log: PathBuf) -> Self {
        Self {
            display: display.to_string(),
            env,
            runner_log,
        }
    }

    fn capture(&self, path: &PathBuf, timeout: Duration) -> Result<(), SessionError> {
        let started = Instant::now();
        let mut last_error = None;

        while started.elapsed() < timeout {
            let output = self
                .command("import")
                .args(["-window", "root"])
                .arg(path)
                .output();

            match output {
                Ok(output) if output.status.success() => match ensure_file_nonempty(path) {
                    Ok(()) => {
                        self.append_log(format!("screenshot captured at '{}'", path.display()))?;
                        return Ok(());
                    }
                    Err(error) => last_error = Some(error.to_string()),
                },
                Ok(output) => {
                    last_error = Some(String::from_utf8_lossy(&output.stderr).to_string())
                }
                Err(error) => last_error = Some(error.to_string()),
            }

            thread::sleep(Duration::from_millis(200));
        }

        Err(SessionError::new(
            FailureReason::ScreenshotFailed,
            format!(
                "failed to capture screenshot within {}s: {}",
                timeout.as_secs(),
                last_error
                    .unwrap_or_else(|| "unknown error".to_string())
                    .trim()
            ),
        ))
    }

    fn detect_app_error_text(&self, path: &Path) -> Result<Option<&'static str>, SessionError> {
        let output = self
            .command("tesseract")
            .arg(path)
            .arg("stdout")
            .args(["--psm", "6"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let marker = app_error_text_marker(&text);
                match marker {
                    Some(marker) => self.append_log(format!(
                        "screenshot OCR matched app-error marker '{marker}'"
                    ))?,
                    None => self.append_log("screenshot OCR found no app-error markers")?,
                }
                Ok(marker)
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to OCR screenshot with tesseract: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: tesseract",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run tesseract for screenshot OCR: {error}"),
            )),
        }
    }

    fn find_button_click_target(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<(i32, i32), SessionError> {
        let button_regions = self.detect_button_regions(path)?;
        let text_center = self.find_text_center(path, text)?;

        button_click_target_for_text(text, text_center, &button_regions)
            .or_else(|_| self.find_button_click_target_by_label(path, text, &button_regions))
    }

    fn find_button_click_target_by_label(
        &self,
        path: &Path,
        text: &str,
        button_regions: &[ButtonRegion],
    ) -> Result<(i32, i32), SessionError> {
        let mut matches = Vec::new();
        for region in button_regions {
            if self.button_region_label_matches(path, region, text)? {
                matches.push(region);
            }
        }

        match matches.as_slice() {
            [button] => Ok(button.center()),
            [] => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("could not find a button labeled '{text}' in screenshot OCR output"),
            )),
            _ => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("found multiple buttons matching label '{text}'"),
            )),
        }
    }

    fn button_region_label_matches(
        &self,
        path: &Path,
        region: &ButtonRegion,
        text: &str,
    ) -> Result<bool, SessionError> {
        let output = self
            .command("convert")
            .arg(path)
            .args([
                "-crop",
                &region.crop_geometry(),
                "-resize",
                "600%",
                "-colorspace",
                "Gray",
                "-normalize",
                "png:-",
            ])
            .output();

        let image = match output {
            Ok(output) if output.status.success() => output.stdout,
            Ok(output) => {
                return Err(SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!(
                        "failed to crop button region with ImageMagick: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::new(
                    FailureReason::DependencyFailed,
                    "missing required tool for click screenshots: convert",
                ));
            }
            Err(error) => {
                return Err(SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!("failed to run ImageMagick for button crop: {error}"),
                ));
            }
        };

        let mut tesseract = self
            .command("tesseract")
            .args(["stdin", "stdout", "--psm", "7"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    SessionError::new(
                        FailureReason::DependencyFailed,
                        "missing required tool: tesseract",
                    )
                } else {
                    SessionError::new(
                        FailureReason::ScreenshotFailed,
                        format!("failed to run tesseract for button label OCR: {error}"),
                    )
                }
            })?;

        if let Some(mut stdin) = tesseract.stdin.take() {
            stdin.write_all(&image).map_err(SessionError::internal)?;
        }

        let output = tesseract
            .wait_with_output()
            .map_err(SessionError::internal)?;
        if !output.status.success() {
            return Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to OCR button label with tesseract: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }

        let label = String::from_utf8_lossy(&output.stdout);
        Ok(ocr_label_matches(&label, text))
    }

    fn find_text_center(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<Option<(i32, i32)>, SessionError> {
        let output = self
            .command("tesseract")
            .arg(path)
            .arg("stdout")
            .args(["--psm", "6", "tsv"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let tsv = String::from_utf8_lossy(&output.stdout);
                Ok(find_ocr_text_center(&tsv, text))
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to locate click text with tesseract: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: tesseract",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run tesseract for click text OCR: {error}"),
            )),
        }
    }

    fn detect_button_regions(&self, path: &Path) -> Result<Vec<ButtonRegion>, SessionError> {
        let mut regions = Vec::new();
        for color in BUTTON_FILL_COLORS {
            regions.extend(self.detect_button_regions_for_color(path, *color)?);
        }
        deduplicate_button_regions(&mut regions);
        self.append_log(format!("detected {} button-like region(s)", regions.len()))?;
        Ok(regions)
    }

    fn detect_button_regions_for_color(
        &self,
        path: &Path,
        color: ButtonFillColor,
    ) -> Result<Vec<ButtonRegion>, SessionError> {
        let output = self
            .command("convert")
            .arg(path)
            .args([
                "-alpha",
                "off",
                "-fuzz",
                color.fuzz,
                "-fill",
                "black",
                "+opaque",
                color.value,
                "-fill",
                "white",
                "-opaque",
                color.value,
                "-define",
                "connected-components:verbose=true",
                "-connected-components",
                "8",
                "null:",
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let output = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                Ok(parse_button_regions(&output))
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to detect button regions with ImageMagick: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: convert",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run ImageMagick for button detection: {error}"),
            )),
        }
    }

    fn click(&self, target: (i32, i32)) -> Result<(), SessionError> {
        let output = self
            .command("xdotool")
            .args([
                "mousemove".to_string(),
                target.0.to_string(),
                target.1.to_string(),
                "click".to_string(),
                "1".to_string(),
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                self.append_log(format!(
                    "clicked screenshot button at {},{}",
                    target.0, target.1
                ))?;
                Ok(())
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to click screenshot button with xdotool: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run xdotool for screenshot button click: {error}"),
            )),
        }
    }

    fn command(&self, program: &str) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.env("DISPLAY", &self.display);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    fn append_log(&self, message: impl AsRef<str>) -> Result<(), SessionError> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.runner_log)
            .and_then(|mut file| writeln!(file, "{}", message.as_ref()))
            .map_err(SessionError::internal)
    }
}

fn app_error_text_marker(text: &str) -> Option<&'static str> {
    let normalized = normalized_ocr_text(text);
    APP_ERROR_TEXT_MARKERS
        .iter()
        .copied()
        .find(|marker| normalized.contains(marker))
}

fn normalized_ocr_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OcrWord {
    text: String,
    block_num: i32,
    par_num: i32,
    line_num: i32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ButtonRegion {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    area: i32,
}

#[derive(Debug, Clone, Copy)]
struct ButtonFillColor {
    value: &'static str,
    fuzz: &'static str,
}

const BUTTON_FILL_COLORS: &[ButtonFillColor] = &[
    ButtonFillColor {
        value: "#3584e4",
        fuzz: "12%",
    },
    ButtonFillColor {
        value: "#99c1f1",
        fuzz: "10%",
    },
    ButtonFillColor {
        value: "#e6e6e7",
        fuzz: "2%",
    },
    ButtonFillColor {
        value: "#deddda",
        fuzz: "4%",
    },
    ButtonFillColor {
        value: "#c0bfbc",
        fuzz: "6%",
    },
    ButtonFillColor {
        value: "#e01b24",
        fuzz: "12%",
    },
];

impl ButtonRegion {
    fn center(&self) -> (i32, i32) {
        (self.left + self.width / 2, self.top + self.height / 2)
    }

    fn crop_geometry(&self) -> String {
        format!("{}x{}+{}+{}", self.width, self.height, self.left, self.top)
    }

    fn contains(&self, point: (i32, i32)) -> bool {
        let padding = 8;
        point.0 >= self.left - padding
            && point.0 <= self.left + self.width + padding
            && point.1 >= self.top - padding
            && point.1 <= self.top + self.height + padding
    }

    fn overlaps_substantially(&self, other: &Self) -> bool {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = (self.left + self.width).min(other.left + other.width);
        let bottom = (self.top + self.height).min(other.top + other.height);
        let overlap_width = (right - left).max(0);
        let overlap_height = (bottom - top).max(0);
        let overlap_area = overlap_width * overlap_height;
        let smaller_area = self.area.min(other.area);
        smaller_area > 0 && overlap_area * 100 / smaller_area >= 90
    }
}

fn parse_button_regions(output: &str) -> Vec<ButtonRegion> {
    output
        .lines()
        .filter_map(parse_connected_component_region)
        .filter(is_button_like_region)
        .collect()
}

fn deduplicate_button_regions(regions: &mut Vec<ButtonRegion>) {
    regions.sort_by_key(|region| (region.top, region.left, region.width, region.height));
    regions.dedup_by(|left, right| left.overlaps_substantially(right));
}

fn button_click_target_for_text(
    text: &str,
    text_center: Option<(i32, i32)>,
    button_regions: &[ButtonRegion],
) -> Result<(i32, i32), SessionError> {
    if let Some(text_center) = text_center {
        return button_regions
            .iter()
            .find(|region| region.contains(text_center))
            .map(ButtonRegion::center)
            .ok_or_else(|| {
                SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!("found text '{text}', but it was not inside a detected button"),
                )
            });
    }

    Err(SessionError::new(
        FailureReason::ScreenshotFailed,
        format!("could not find text '{text}' in screenshot OCR output"),
    ))
}

fn parse_connected_component_region(line: &str) -> Option<ButtonRegion> {
    let geometry = line.split_whitespace().nth(1)?;
    let area = line.split_whitespace().nth(3)?.parse().ok()?;
    let (width, rest) = geometry.split_once('x')?;
    let (height, rest) = rest.split_once('+')?;
    let (left, top) = rest.split_once('+')?;
    Some(ButtonRegion {
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        left: left.parse().ok()?,
        top: top.parse().ok()?,
        area,
    })
}

fn is_button_like_region(region: &ButtonRegion) -> bool {
    let aspect = region.width as f32 / region.height.max(1) as f32;
    region.width >= 100 && (28..=80).contains(&region.height) && aspect >= 3.4
}

fn find_ocr_text_center(tsv: &str, text: &str) -> Option<(i32, i32)> {
    let needle = normalized_ocr_words(text);
    if needle.is_empty() {
        return None;
    }
    let words = parse_ocr_words(tsv);
    words
        .chunk_by(|left, right| left.is_same_line(right))
        .find_map(|line| find_ocr_text_center_in_line(line, &needle))
}

fn find_ocr_text_center_in_line(line: &[OcrWord], needle: &[String]) -> Option<(i32, i32)> {
    line.windows(needle.len()).find_map(|window| {
        window
            .iter()
            .map(|word| word.text.as_str())
            .eq(needle.iter().map(String::as_str))
            .then(|| center_of_words(window))
    })
}

fn parse_ocr_words(tsv: &str) -> Vec<OcrWord> {
    tsv.lines()
        .skip(1)
        .filter_map(|line| {
            let columns: Vec<_> = line.split('\t').collect();
            let text = columns.get(11)?.trim();
            if text.is_empty() {
                return None;
            }
            let text = normalize_ocr_word(text);
            if text.is_empty() {
                return None;
            }
            Some(OcrWord {
                text,
                block_num: columns.get(2)?.parse().ok()?,
                par_num: columns.get(3)?.parse().ok()?,
                line_num: columns.get(4)?.parse().ok()?,
                left: columns.get(6)?.parse().ok()?,
                top: columns.get(7)?.parse().ok()?,
                width: columns.get(8)?.parse().ok()?,
                height: columns.get(9)?.parse().ok()?,
            })
        })
        .collect()
}

impl OcrWord {
    fn is_same_line(&self, other: &Self) -> bool {
        self.block_num == other.block_num
            && self.par_num == other.par_num
            && self.line_num == other.line_num
    }
}

fn center_of_words(words: &[OcrWord]) -> (i32, i32) {
    let left = words.iter().map(|word| word.left).min().unwrap_or_default();
    let top = words.iter().map(|word| word.top).min().unwrap_or_default();
    let right = words
        .iter()
        .map(|word| word.left + word.width)
        .max()
        .unwrap_or_default();
    let bottom = words
        .iter()
        .map(|word| word.top + word.height)
        .max()
        .unwrap_or_default();
    ((left + right) / 2, (top + bottom) / 2)
}

fn normalized_ocr_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize_ocr_word)
        .filter(|word| !word.is_empty())
        .collect()
}

fn normalize_ocr_word(text: &str) -> String {
    text.trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase()
}

fn ocr_label_matches(actual: &str, expected: &str) -> bool {
    let actual = normalize_ocr_label(actual);
    let expected = normalize_ocr_label(expected);
    if actual.is_empty() || expected.is_empty() {
        return false;
    }
    actual.contains(&expected) || levenshtein_distance(&actual, &expected) <= 1
}

fn normalize_ocr_label(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<_> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.chars().enumerate() {
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            current[right_index + 1] = insertion.min(deletion).min(substitution);
        }
        previous.clone_from(&current);
    }

    previous[right.len()]
}

fn click_screenshot_name(index: usize, click_text: &str) -> String {
    format!("{index:03}-after-click-{}.png", screenshot_slug(click_text))
}

fn screenshot_slug(text: &str) -> String {
    let slug = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "text".to_string()
    } else {
        slug
    }
}

const APP_ERROR_TEXT_MARKERS: &[&str] = &[
    "secret portal error",
    "unexpected error",
    "fatal error",
    "unhandled exception",
    "application error",
    "something went wrong",
];

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_keyring_unlock_daemons(env: &[(OsString, OsString)]) {
    let Some(runtime_dir) = env_value(env, "XDG_RUNTIME_DIR") else {
        return;
    };
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        let proc_dir = entry.path();
        let Ok(cmdline) = fs::read(proc_dir.join("cmdline")) else {
            continue;
        };
        if !is_keyring_unlock_cmdline(&cmdline) {
            continue;
        }
        let Ok(environ) = fs::read(proc_dir.join("environ")) else {
            continue;
        };
        if environ_contains(&environ, "XDG_RUNTIME_DIR", runtime_dir) {
            // The unlock helper daemonizes outside dbus-run-session; clean up only
            // the daemon tied to this run's private runtime directory.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

#[cfg(not(unix))]
fn terminate_keyring_unlock_daemons(_env: &[(OsString, OsString)]) {}

fn env_value<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_os_str())
}

#[cfg(unix)]
fn is_keyring_unlock_cmdline(cmdline: &[u8]) -> bool {
    let mut parts = cmdline
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    let Some(program) = parts.next() else {
        return false;
    };
    let Some(program_name) = program.rsplit(|byte| *byte == b'/').next() else {
        return false;
    };
    if program_name != b"gnome-keyring-daemon" {
        return false;
    }

    let args: Vec<_> = parts.collect();
    args.contains(&b"--unlock".as_slice()) && args.contains(&b"--components=secrets".as_slice())
}

#[cfg(unix)]
fn environ_contains(environ: &[u8], key: &str, value: &OsStr) -> bool {
    let mut expected = Vec::from(key.as_bytes());
    expected.push(b'=');
    expected.extend(value.as_bytes());

    environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected.as_slice())
}

fn flatpak_run_target(app_ref: &str) -> String {
    let parts: Vec<_> = app_ref.split('/').collect();
    match parts.as_slice() {
        ["app", app_id, _arch, branch] => format!("{app_id}//{branch}"),
        _ => app_ref.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_full_app_ref_to_flatpak_run_target() {
        assert_eq!(
            flatpak_run_target("app/org.example.App/x86_64/stable"),
            "org.example.App//stable"
        );
        assert_eq!(flatpak_run_target("org.example.App"), "org.example.App");
    }

    #[test]
    fn display_lease_skips_locked_display() {
        let temp = tempfile::tempdir().unwrap();
        let first = DisplayLease::acquire_in(temp.path(), 30000..30002).unwrap();
        let second = DisplayLease::acquire_in(temp.path(), 30000..30002).unwrap();

        assert_eq!(first.display(), ":30000");
        assert_eq!(second.display(), ":30001");
    }

    #[test]
    fn parses_visible_window_ids_from_xdotool_output() {
        assert_eq!(
            visible_window_ids(b"12345\n\n67890\n"),
            vec!["12345".to_string(), "67890".to_string()]
        );
    }

    #[test]
    fn detects_generic_error_window_titles() {
        assert!(is_app_error_window_title("Error"));
        assert!(is_app_error_window_title(" error "));
        assert!(!is_app_error_window_title("Fractal"));
        assert!(!is_app_error_window_title("Errors"));
    }

    #[test]
    fn detects_app_error_text_markers_from_ocr_text() {
        assert_eq!(
            app_error_text_marker("Secret\nPortal   Error"),
            Some("secret portal error")
        );
        assert_eq!(
            app_error_text_marker("An unexpected error occurred"),
            Some("unexpected error")
        );
        assert_eq!(app_error_text_marker("Error handling preferences"), None);
    }

    #[test]
    fn locates_click_text_from_tesseract_tsv() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t20\t50\t12\t95\tOpen\n\
5\t1\t1\t1\t1\t2\t70\t20\t80\t12\t95\tPreferences\n";

        assert_eq!(
            find_ocr_text_center(tsv, "Open Preferences"),
            Some((80, 26))
        );
        assert_eq!(find_ocr_text_center(tsv, "Missing"), None);
    }

    #[test]
    fn does_not_match_click_text_across_ocr_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t10\t20\t50\t12\t95\tOpen\n\
5\t1\t1\t1\t2\t1\t70\t60\t80\t12\t95\tPreferences\n";

        assert_eq!(find_ocr_text_center(tsv, "Open Preferences"), None);
    }

    #[test]
    fn builds_stable_click_screenshot_names() {
        assert_eq!(
            click_screenshot_name(1, "Open Preferences"),
            "001-after-click-open-preferences.png"
        );
        assert_eq!(click_screenshot_name(2, "???"), "002-after-click-text.png");
    }

    #[test]
    fn parses_button_like_regions_from_imagemagick_output() {
        let output = "Objects (id: bounding-box centroid area mean-color):\n  0: 1280x720+0+0 639.4,357.8 906233 srgb(0,0,0)\n  10: 260x44+510+510 639.6,531.5 10724 srgb(255,255,255)\n  1: 148x47+555+235 619.8,250.6 2899 srgb(255,255,255)\n";

        assert_eq!(
            parse_button_regions(output),
            vec![ButtonRegion {
                left: 510,
                top: 510,
                width: 260,
                height: 44,
                area: 10724,
            }]
        );
    }

    #[test]
    fn accepts_common_gray_button_dimensions() {
        let output = "Objects (id: bounding-box centroid area mean-color):\n  7: 121x35+579+601 639.0,618.0 4235 srgb(255,255,255)\n";

        assert_eq!(
            parse_button_regions(output),
            vec![ButtonRegion {
                left: 579,
                top: 601,
                width: 121,
                height: 35,
                area: 4235,
            }]
        );
    }

    #[test]
    fn deduplicates_overlapping_button_regions() {
        let mut regions = vec![
            ButtonRegion {
                left: 510,
                top: 510,
                width: 260,
                height: 44,
                area: 10724,
            },
            ButtonRegion {
                left: 512,
                top: 511,
                width: 256,
                height: 42,
                area: 10300,
            },
        ];

        deduplicate_button_regions(&mut regions);

        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn clicks_button_center_for_text_inside_button() {
        let regions = vec![ButtonRegion {
            left: 510,
            top: 510,
            width: 260,
            height: 44,
            area: 10724,
        }];

        assert_eq!(
            button_click_target_for_text("Log In", Some((640, 532)), &regions).unwrap(),
            (640, 532)
        );
    }

    #[test]
    fn rejects_text_outside_detected_buttons() {
        let regions = vec![ButtonRegion {
            left: 510,
            top: 510,
            width: 260,
            height: 44,
            area: 10724,
        }];

        let error = button_click_target_for_text("Welcome to Fractal", Some((638, 66)), &regions)
            .unwrap_err();

        assert!(error.message.contains("not inside a detected button"));
    }

    #[test]
    fn rejects_missing_label_even_with_one_button() {
        let regions = vec![ButtonRegion {
            left: 510,
            top: 510,
            width: 260,
            height: 44,
            area: 10724,
        }];

        let error = button_click_target_for_text("Log In", None, &regions).unwrap_err();

        assert!(error.message.contains("could not find text"));
    }

    #[test]
    fn fuzzy_matches_cropped_button_ocr_labels() {
        assert!(ocr_label_matches("togin", "Log In"));
        assert!(ocr_label_matches("Advanced...", "Advanced"));
        assert!(!ocr_label_matches("Cancel", "Log In"));
    }

    #[test]
    fn rejects_missing_label_when_buttons_are_ambiguous() {
        let regions = vec![
            ButtonRegion {
                left: 460,
                top: 500,
                width: 140,
                height: 40,
                area: 5200,
            },
            ButtonRegion {
                left: 620,
                top: 500,
                width: 140,
                height: 40,
                area: 5200,
            },
        ];

        let error = button_click_target_for_text("Continue", None, &regions).unwrap_err();

        assert!(error.message.contains("could not find text"));
    }

    #[cfg(unix)]
    #[test]
    fn matches_only_keyring_unlock_daemon_cmdlines() {
        assert!(is_keyring_unlock_cmdline(
            b"/usr/bin/gnome-keyring-daemon\0--unlock\0--components=secrets\0"
        ));
        assert!(!is_keyring_unlock_cmdline(
            b"sh\0-c\0gnome-keyring-daemon --unlock --components=secrets\0"
        ));
        assert!(!is_keyring_unlock_cmdline(
            b"/usr/bin/gnome-keyring-daemon\0--start\0--foreground\0--components=secrets\0"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn matches_environ_entries_exactly() {
        assert!(environ_contains(
            b"A=1\0XDG_RUNTIME_DIR=/tmp/flatpak-smoke\0",
            "XDG_RUNTIME_DIR",
            OsStr::new("/tmp/flatpak-smoke")
        ));
        assert!(!environ_contains(
            b"XDG_RUNTIME_DIR=/tmp/flatpak-smoke-other\0",
            "XDG_RUNTIME_DIR",
            OsStr::new("/tmp/flatpak-smoke")
        ));
    }
}
