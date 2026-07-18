use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

use aes::{
    Aes128,
    cipher::{Block, BlockEncrypt, KeyInit},
};
use md5::{Digest, Md5};
use num_bigint::BigUint;

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
        let display = WAYLAND_DISPLAY.to_string();
        let vnc_port_lease = VncPortLease::acquire()?;
        let vnc_port = vnc_port_lease.port();
        self.layout
            .append_runner_log(format!(
                "starting Weston Wayland display {display} with VNC input port {vnc_port}"
            ))
            .map_err(SessionError::internal)?;

        let mut weston = self.start_weston(&display, vnc_port)?;
        let session_result = self.run_app_session(
            app_ref,
            screenshot_name,
            screenshots_after_click,
            &display,
            vnc_port,
        );
        terminate_child(&mut weston);
        session_result
    }

    fn run_app_session(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        screenshots_after_click: &[String],
        display: &str,
        vnc_port: u16,
    ) -> Result<SessionSuccess, SessionError> {
        let launch_started = Instant::now();
        let screenshotter = Screenshotter::new(
            display,
            vnc_port,
            self.env.clone(),
            self.layout.runner_log.clone(),
        );
        let mut session_client = VncClient::connect(vnc_port)?;
        let baseline_path = self.layout.logs_dir.join("wayland-baseline.png");
        screenshotter.capture_once_with_client(&mut session_client, &baseline_path)?;

        let mut app = self.spawn_app(app_ref, display)?;
        let result = (|| {
            self.wait_for_app_frame(
                &mut app,
                &screenshotter,
                &mut session_client,
                &baseline_path,
                self.bounded_timeout(self.window_timeout)?,
            )?;
            let launch_to_window = launch_started.elapsed().as_millis();

            if let Some(status) = app.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::EarlyExit,
                    format!("app exited before screenshot with {status}"),
                ));
            }

            thread::sleep(Duration::from_millis(500));

            let mut screenshot_path = self.layout.screenshot_path(screenshot_name);
            let relative_screenshot_path = self.layout.relative_screenshot_path(screenshot_name);
            let mut screenshots = Vec::new();
            self.capture_required_screenshot(
                &screenshotter,
                &mut session_client,
                &screenshot_path,
                relative_screenshot_path,
                self.bounded_timeout(self.screenshot_timeout)?,
                &mut screenshots,
            )?;
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
                    .find_text_center(&screenshot_path, click_text)?
                    .ok_or_else(|| {
                        SessionError::new(
                            FailureReason::ScreenshotFailed,
                            format!("could not find text '{click_text}' in screenshot OCR output"),
                        )
                    })
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?;
                screenshotter
                    .click_with_client(&mut session_client, click_target)
                    .map_err(|error| error.with_screenshots(screenshots.clone()))?;
                thread::sleep(Duration::from_millis(500));

                let next_path = self.capture_changed_screenshot(
                    &screenshotter,
                    &mut session_client,
                    &screenshot_path,
                    (index + 1, click_text),
                    self.bounded_timeout(self.screenshot_timeout)?,
                    &mut screenshots,
                )?;
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

    fn capture_required_screenshot(
        &self,
        screenshotter: &Screenshotter,
        client: &mut VncClient,
        path: &Path,
        relative_path: String,
        timeout: Duration,
        screenshots: &mut Vec<String>,
    ) -> Result<(), SessionError> {
        screenshotter.capture_with_client(client, path, timeout)?;
        screenshots.push(relative_path);
        if screenshotter.screenshot_has_content(path)? {
            Ok(())
        } else {
            Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "screenshot '{}' contained no visible app content",
                    path.display()
                ),
            )
            .with_screenshots(screenshots.clone()))
        }
    }

    fn capture_changed_screenshot(
        &self,
        screenshotter: &Screenshotter,
        client: &mut VncClient,
        previous_path: &Path,
        click: (usize, &str),
        timeout: Duration,
        screenshots: &mut Vec<String>,
    ) -> Result<PathBuf, SessionError> {
        let (index, click_text) = click;
        let next_name = click_screenshot_name(index, click_text);
        let next_path = self.layout.screenshot_path(&next_name);
        let next_relative_path = self.layout.relative_screenshot_path(&next_name);
        let started = Instant::now();
        let mut last_error = None;
        while started.elapsed() < timeout {
            match screenshotter.capture_once_with_client(client, &next_path) {
                Ok(()) => {
                    let has_content = screenshotter.screenshot_has_content(&next_path)?;
                    let changed =
                        screenshotter.screenshots_differ_after_click(previous_path, &next_path)?;
                    if has_content && changed {
                        screenshots.push(next_relative_path);
                        return Ok(next_path);
                    }
                    last_error = Some("screenshot did not change after click".to_string());
                }
                Err(error) => last_error = Some(error.message),
            }

            thread::sleep(Duration::from_millis(200));
        }

        Err(SessionError::new(
            FailureReason::ScreenshotFailed,
            format!(
                "failed to capture screenshot after clicking '{click_text}' within {}s: {}",
                timeout.as_secs(),
                last_error
                    .unwrap_or_else(|| "unknown error".to_string())
                    .trim(),
            ),
        )
        .with_screenshots(screenshots.clone()))
    }

    fn start_weston(&self, display: &str, vnc_port: u16) -> Result<Child, SessionError> {
        let mut last_error = None;
        for backend in ["vnc", "vnc-backend.so"] {
            let mut weston = self.spawn_weston(display, backend, vnc_port)?;
            match self.wait_for_compositor(display, vnc_port, &mut weston) {
                Ok(()) => return Ok(weston),
                Err(error) => {
                    terminate_child(&mut weston);
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            SessionError::new(FailureReason::DisplayStartFailed, "failed to start Weston")
        }))
    }

    fn spawn_weston(
        &self,
        display: &str,
        backend: &str,
        vnc_port: u16,
    ) -> Result<Child, SessionError> {
        let stdout = File::create(self.layout.logs_dir.join("weston.stdout.log"))
            .map_err(SessionError::internal)?;
        let stderr = File::create(self.layout.logs_dir.join("weston.stderr.log"))
            .map_err(SessionError::internal)?;
        self.base_command("weston", display)
            .arg(format!("--backend={backend}"))
            .arg(format!("--socket={display}"))
            .arg("--address=127.0.0.1")
            .arg(format!("--port={vnc_port}"))
            .args([
                "--width=1280",
                "--height=720",
                "--idle-time=0",
                "--debug",
                "--disable-transport-layer-security",
            ])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("failed to start Weston with backend '{backend}': {error}"),
                )
            })
    }

    fn wait_for_compositor(
        &self,
        display: &str,
        vnc_port: u16,
        weston: &mut Child,
    ) -> Result<(), SessionError> {
        let timeout = self.bounded_timeout(self.display_timeout)?;
        let started = Instant::now();
        let mut last_error = None;
        let screenshotter = Screenshotter::new(
            display,
            vnc_port,
            self.env.clone(),
            self.layout.runner_log.clone(),
        );
        let readiness_path = self.layout.logs_dir.join("wayland-readiness.png");
        while started.elapsed() < timeout {
            if let Some(status) = weston.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("Weston exited before the Wayland display became ready with {status}"),
                ));
            }

            match screenshotter.capture_once(&readiness_path) {
                Ok(()) => {
                    return match weston.try_wait().map_err(SessionError::internal)? {
                        Some(status) => Err(SessionError::new(
                            FailureReason::DisplayStartFailed,
                            format!("Weston exited after display readiness check with {status}"),
                        )),
                        None => Ok(()),
                    };
                }
                Err(error) => last_error = Some(error.message),
            }

            thread::sleep(Duration::from_millis(100));
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!(
                "Weston Wayland display did not become ready within {}s: {}",
                timeout.as_secs(),
                last_error
                    .unwrap_or_else(|| "unknown readiness error".to_string())
                    .trim()
            ),
        ))
    }

    fn wait_for_app_frame(
        &self,
        app: &mut Child,
        screenshotter: &Screenshotter,
        client: &mut VncClient,
        baseline_path: &Path,
        timeout: Duration,
    ) -> Result<(), SessionError> {
        let started = Instant::now();
        let candidate_path = self.layout.logs_dir.join("wayland-window-detection.png");
        while started.elapsed() < timeout {
            if let Some(status) = app.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::EarlyExit,
                    format!("app exited before a visible Wayland frame appeared with {status}"),
                ));
            }

            screenshotter.capture_once_with_client(client, &candidate_path)?;
            if screenshotter.screenshots_differ(baseline_path, &candidate_path)?
                && screenshotter.screenshot_has_content(&candidate_path)?
            {
                self.layout
                    .append_runner_log(
                        "visible Wayland frame detected from non-empty screenshot content",
                    )
                    .map_err(SessionError::internal)?;
                return Ok(());
            }

            thread::sleep(Duration::from_millis(200));
        }

        Err(SessionError::new(
            FailureReason::WindowTimeout,
            format!(
                "no visible Wayland frame appeared within {}s",
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
                "--env=GDK_BACKEND=wayland",
                "--env=QT_QPA_PLATFORM=wayland",
                "--env=SDL_VIDEODRIVER=wayland",
                "--env=CLUTTER_BACKEND=wayland",
                "--env=MOZ_ENABLE_WAYLAND=1",
                "--env=XDG_SESSION_TYPE=wayland",
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
        command.env_remove("DISPLAY");
        command.env_remove("WAYLAND_DISPLAY");
        command.env("WAYLAND_DISPLAY", display);
        command.env("XDG_SESSION_TYPE", "wayland");
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }
}

const WAYLAND_DISPLAY: &str = "flatpak-smoke-wayland";

const START_DESKTOP_SERVICES_AND_RUN_FLATPAK: &str = r#"
set -eu
printf '\n' | gnome-keyring-daemon --unlock --components=secrets >/dev/null
exec flatpak run "$@"
"#;

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

struct Screenshotter {
    display: String,
    vnc_port: u16,
    env: Vec<(OsString, OsString)>,
    runner_log: PathBuf,
}

impl Screenshotter {
    fn new(
        display: &str,
        vnc_port: u16,
        env: Vec<(OsString, OsString)>,
        runner_log: PathBuf,
    ) -> Self {
        Self {
            display: display.to_string(),
            vnc_port,
            env,
            runner_log,
        }
    }

    fn capture_with_client(
        &self,
        client: &mut VncClient,
        path: &Path,
        timeout: Duration,
    ) -> Result<(), SessionError> {
        let started = Instant::now();
        let mut last_error = None;

        while started.elapsed() < timeout {
            match self.capture_once_with_client(client, path) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error.message),
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

    fn capture_once(&self, path: &Path) -> Result<(), SessionError> {
        let mut client = VncClient::connect(self.vnc_port)?;
        client.capture_png(path)?;
        self.append_log(format!("screenshot captured at '{}'", path.display()))?;
        Ok(())
    }

    fn click_with_client(
        &self,
        client: &mut VncClient,
        target: (i32, i32),
    ) -> Result<(), SessionError> {
        client.click(target)?;
        self.append_log(format!(
            "clicked screenshot target at {},{}",
            target.0, target.1
        ))?;
        Ok(())
    }

    fn capture_once_with_client(
        &self,
        client: &mut VncClient,
        path: &Path,
    ) -> Result<(), SessionError> {
        client.capture_png(path)?;
        self.append_log(format!("screenshot captured at '{}'", path.display()))?;
        Ok(())
    }

    fn screenshots_differ(&self, baseline: &Path, candidate: &Path) -> Result<bool, SessionError> {
        self.screenshots_differ_by_threshold(baseline, candidate, SCREENSHOT_DIFF_PIXEL_THRESHOLD)
    }

    fn screenshots_differ_after_click(
        &self,
        baseline: &Path,
        candidate: &Path,
    ) -> Result<bool, SessionError> {
        self.screenshots_differ_by_threshold(
            baseline,
            candidate,
            POST_CLICK_SCREENSHOT_DIFF_PIXEL_THRESHOLD,
        )
    }

    fn screenshots_differ_by_threshold(
        &self,
        baseline: &Path,
        candidate: &Path,
        pixel_threshold: u64,
    ) -> Result<bool, SessionError> {
        let output = self
            .command("compare")
            .args(["-metric", "AE"])
            .arg(baseline)
            .arg(candidate)
            .arg("null:")
            .output();

        match output {
            Ok(output) if output.status.success() => Ok(false),
            Ok(output) if output.status.code() == Some(1) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(parse_absolute_error_metric(&stderr)
                    .map(|different_pixels| {
                        screenshot_diff_exceeds_threshold(different_pixels, pixel_threshold)
                    })
                    .unwrap_or(true))
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to compare screenshots with ImageMagick: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: compare",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run ImageMagick compare: {error}"),
            )),
        }
    }

    fn screenshot_has_content(&self, path: &Path) -> Result<bool, SessionError> {
        let output = self
            .command("identify")
            .args(["-format", "%[fx:standard_deviation]"])
            .arg(path)
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                parse_standard_deviation(&stdout)
                    .map(|value| value > SCREENSHOT_CONTENT_STANDARD_DEVIATION_THRESHOLD)
                    .ok_or_else(|| {
                        SessionError::new(
                            FailureReason::ScreenshotFailed,
                            format!(
                                "failed to parse ImageMagick standard deviation: {}",
                                stdout.trim()
                            ),
                        )
                    })
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to inspect screenshot with ImageMagick: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: identify",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run ImageMagick identify: {error}"),
            )),
        }
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
                let matches = find_ocr_text_matches(&tsv, text);
                if let Some(center) = self.find_primary_action_button_center(path, text)? {
                    return Ok(Some(center));
                }
                unambiguous_ocr_text_center(&matches, text)
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

    fn find_primary_action_button_center(
        &self,
        path: &Path,
        text: &str,
    ) -> Result<Option<(i32, i32)>, SessionError> {
        let output = self
            .command("convert")
            .arg(path)
            .args([
                "-alpha",
                "off",
                "-fuzz",
                "18%",
                "-fill",
                "black",
                "+opaque",
                "#3584e4",
                "-fill",
                "white",
                "-opaque",
                "#3584e4",
                "-define",
                "connected-components:verbose=true",
                "-connected-components",
                "8",
                "null:",
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let components = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let Some(button) = find_primary_action_button(&components) else {
                    return Ok(None);
                };
                if !self.primary_action_button_matches_label(path, button, text)? {
                    return Ok(None);
                }
                let center = button.center();
                self.append_log(format!(
                    "using matching primary action button visual fallback for click text '{text}' at {},{}",
                    center.0, center.1
                ))?;
                Ok(Some(center))
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to locate primary action button with ImageMagick: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: convert",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run ImageMagick convert for button detection: {error}"),
            )),
        }
    }

    fn primary_action_button_matches_label(
        &self,
        path: &Path,
        button: ComponentBox,
        text: &str,
    ) -> Result<bool, SessionError> {
        let temp = tempfile::tempdir().map_err(SessionError::internal)?;
        let crop_path = temp.path().join("primary-action-button.png");
        let crop_geometry = button.crop_geometry();
        let output = self
            .command("convert")
            .arg(path)
            .args(["-crop", &crop_geometry])
            .args(["-resize", "400%", "-alpha", "off", "-colorspace", "Gray"])
            .arg(&crop_path)
            .output();

        match output {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Err(SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!(
                        "failed to crop primary action button with ImageMagick: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::new(
                    FailureReason::DependencyFailed,
                    "missing required tool: convert",
                ));
            }
            Err(error) => {
                return Err(SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!("failed to run ImageMagick convert for button crop: {error}"),
                ));
            }
        }

        let output = self
            .command("tesseract")
            .arg(&crop_path)
            .arg("stdout")
            .args(["--psm", "7"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let ocr_text = String::from_utf8_lossy(&output.stdout);
                Ok(ocr_text_contains_label(&ocr_text, text))
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to OCR primary action button crop with tesseract: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: tesseract",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run tesseract for button crop OCR: {error}"),
            )),
        }
    }

    fn command(&self, program: &str) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.env_remove("DISPLAY");
        command.env_remove("WAYLAND_DISPLAY");
        command.env("WAYLAND_DISPLAY", &self.display);
        command.env("XDG_SESSION_TYPE", "wayland");
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

fn find_ocr_text_matches(tsv: &str, text: &str) -> Vec<(i32, i32)> {
    let needle = normalized_ocr_words(text);
    if needle.is_empty() {
        return Vec::new();
    }
    let words = parse_ocr_words(tsv);
    words
        .chunk_by(|left, right| left.is_same_line(right))
        .flat_map(|line| find_ocr_text_matches_in_line(line, &needle))
        .collect()
}

fn find_ocr_text_matches_in_line(line: &[OcrWord], needle: &[String]) -> Vec<(i32, i32)> {
    line.windows(needle.len())
        .filter(|window| {
            window
                .iter()
                .map(|word| word.text.as_str())
                .eq(needle.iter().map(String::as_str))
        })
        .map(center_of_words)
        .collect()
}

fn unambiguous_ocr_text_center(
    matches: &[(i32, i32)],
    text: &str,
) -> Result<Option<(i32, i32)>, SessionError> {
    match matches {
        [] => Ok(None),
        [center] => Ok(Some(*center)),
        _ => Err(SessionError::new(
            FailureReason::ScreenshotFailed,
            format!(
                "click text '{text}' matched multiple OCR locations and no matching primary action button was found"
            ),
        )),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComponentBox {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    area: i32,
}

fn find_primary_action_button(components: &str) -> Option<ComponentBox> {
    let candidates: Vec<_> = components
        .lines()
        .filter_map(parse_connected_component)
        .filter(ComponentBox::is_primary_action_button_like)
        .collect();

    if candidates.len() != 1 {
        return None;
    }

    Some(candidates[0])
}

fn parse_connected_component(line: &str) -> Option<ComponentBox> {
    let mut parts = line.split_whitespace();
    parts.next()?.strip_suffix(':')?.parse::<usize>().ok()?;
    let bbox = parse_component_bbox(parts.next()?)?;
    let _centroid = parts.next()?;
    let area = parts.next()?.parse().ok()?;
    Some(ComponentBox { area, ..bbox })
}

fn parse_component_bbox(value: &str) -> Option<ComponentBox> {
    let (width, rest) = value.split_once('x')?;
    let (height, rest) = rest.split_once('+')?;
    let (x, y) = rest.split_once('+')?;
    Some(ComponentBox {
        x: x.parse().ok()?,
        y: y.parse().ok()?,
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        area: 0,
    })
}

impl ComponentBox {
    fn center(self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    fn crop_geometry(&self) -> String {
        format!("{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }

    fn is_primary_action_button_like(&self) -> bool {
        if self.width < 80 || self.width > 640 || self.height < 24 || self.height > 96 {
            return false;
        }
        if self.width < self.height * 2 {
            return false;
        }
        self.area * 100 >= self.width * self.height * 45
    }
}

fn ocr_text_contains_label(ocr_text: &str, label: &str) -> bool {
    let haystack = normalized_ocr_words(ocr_text);
    let needle = normalized_ocr_words(label);
    if needle.is_empty() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
}

const SCREENSHOT_DIFF_PIXEL_THRESHOLD: u64 = 1024;
const POST_CLICK_SCREENSHOT_DIFF_PIXEL_THRESHOLD: u64 = 25;
const SCREENSHOT_CONTENT_STANDARD_DEVIATION_THRESHOLD: f64 = 0.01;
const POINTER_PARK_POSITION: (i32, i32) = (1, 1);
const POINTER_PARK_SETTLE_MS: u64 = 75;

fn parse_absolute_error_metric(output: &str) -> Option<u64> {
    output
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
        .map(|value| value.round() as u64)
}

fn screenshot_diff_exceeds_threshold(different_pixels: u64, threshold: u64) -> bool {
    different_pixels > threshold
}

fn parse_standard_deviation(output: &str) -> Option<f64> {
    output.trim().parse().ok()
}

struct VncPortLease {
    port: u16,
    lock_path: PathBuf,
}

impl VncPortLease {
    fn acquire() -> Result<Self, SessionError> {
        Self::acquire_in(
            &std::env::temp_dir().join("flatpak-smoke-vnc-ports"),
            5900..6000,
        )
    }

    fn acquire_in(root: &Path, ports: std::ops::Range<u16>) -> Result<Self, SessionError> {
        fs::create_dir_all(root).map_err(SessionError::internal)?;

        for port in ports {
            if TcpListener::bind(("127.0.0.1", port)).is_err() {
                continue;
            }

            let lock_path = root.join(format!("port-{port}.lock"));
            Self::remove_stale_lock(&lock_path);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut lock_file) => {
                    writeln!(lock_file, "{}", std::process::id())
                        .map_err(SessionError::internal)?;
                    return Ok(Self { port, lock_path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(SessionError::internal(error)),
            }
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            "no free VNC port found in range 5900-5999",
        ))
    }

    fn port(&self) -> u16 {
        self.port
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

impl Drop for VncPortLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

struct VncClient {
    stream: TcpStream,
    width: u16,
    height: u16,
    pixel_format: VncPixelFormat,
}

impl VncClient {
    fn connect(port: u16) -> Result<Self, SessionError> {
        let address: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut stream =
            TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|error| {
                SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("failed to connect to Weston VNC port {port}: {error}"),
                )
            })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(SessionError::internal)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(SessionError::internal)?;

        let mut version = [0; 12];
        stream.read_exact(&mut version).map_err(vnc_read_error)?;
        stream.write_all(&version).map_err(SessionError::internal)?;
        perform_vnc_security_handshake(&mut stream, &version)?;
        stream.write_all(&[1]).map_err(SessionError::internal)?;

        let mut server_init = [0; 24];
        stream
            .read_exact(&mut server_init)
            .map_err(vnc_read_error)?;
        let width = u16::from_be_bytes([server_init[0], server_init[1]]);
        let height = u16::from_be_bytes([server_init[2], server_init[3]]);
        let pixel_format = VncPixelFormat::parse(&server_init[4..20])?;
        let name_len = u32::from_be_bytes([
            server_init[20],
            server_init[21],
            server_init[22],
            server_init[23],
        ]) as usize;
        let mut name = vec![0; name_len];
        stream.read_exact(&mut name).map_err(vnc_read_error)?;

        let mut client = Self {
            stream,
            width,
            height,
            pixel_format,
        };
        client.set_raw_encoding()?;
        Ok(client)
    }

    fn capture_png(&mut self, path: &Path) -> Result<(), SessionError> {
        self.move_pointer(POINTER_PARK_POSITION)?;
        thread::sleep(Duration::from_millis(POINTER_PARK_SETTLE_MS));

        self.request_framebuffer_update()?;
        let rgb = self.read_framebuffer_update()?;
        let temp = tempfile::tempdir().map_err(SessionError::internal)?;
        let ppm_path = temp.path().join("frame.ppm");
        write_ppm(&ppm_path, self.width, self.height, &rgb)?;

        let output = std::process::Command::new("convert")
            .arg(&ppm_path)
            .arg(path)
            .output();
        match output {
            Ok(output) if output.status.success() => {
                ensure_file_nonempty(path).map_err(SessionError::internal)?;
                Ok(())
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to convert VNC framebuffer to PNG: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: convert",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run ImageMagick convert: {error}"),
            )),
        }
    }

    fn click(&mut self, target: (i32, i32)) -> Result<(), SessionError> {
        let x = target.0.clamp(0, i32::from(self.width.saturating_sub(1))) as u16;
        let y = target.1.clamp(0, i32::from(self.height.saturating_sub(1))) as u16;
        self.pointer_event(0, x, y)?;
        thread::sleep(Duration::from_millis(150));
        self.pointer_event(1, x, y)?;
        thread::sleep(Duration::from_millis(100));
        self.pointer_event(0, x, y)?;
        thread::sleep(Duration::from_millis(500));
        Ok(())
    }

    fn move_pointer(&mut self, target: (i32, i32)) -> Result<(), SessionError> {
        let x = target.0.clamp(0, i32::from(self.width.saturating_sub(1))) as u16;
        let y = target.1.clamp(0, i32::from(self.height.saturating_sub(1))) as u16;
        self.pointer_event(0, x, y)
    }

    fn set_raw_encoding(&mut self) -> Result<(), SessionError> {
        let mut message = Vec::with_capacity(8);
        message.extend([2, 0]);
        message.extend(1u16.to_be_bytes());
        message.extend(0i32.to_be_bytes());
        self.stream
            .write_all(&message)
            .map_err(SessionError::internal)
    }

    fn request_framebuffer_update(&mut self) -> Result<(), SessionError> {
        let mut message = Vec::with_capacity(10);
        message.extend([3, 0]);
        message.extend(0u16.to_be_bytes());
        message.extend(0u16.to_be_bytes());
        message.extend(self.width.to_be_bytes());
        message.extend(self.height.to_be_bytes());
        self.stream
            .write_all(&message)
            .map_err(SessionError::internal)
    }

    fn read_framebuffer_update(&mut self) -> Result<Vec<u8>, SessionError> {
        loop {
            let message_type = self.read_u8()?;
            match message_type {
                0 => return self.read_framebuffer_rectangles(),
                2 => continue,
                3 => {
                    self.read_padding(3)?;
                    let len = self.read_u32()? as usize;
                    let mut text = vec![0; len];
                    self.stream.read_exact(&mut text).map_err(vnc_read_error)?;
                }
                value => {
                    return Err(SessionError::new(
                        FailureReason::ScreenshotFailed,
                        format!("unexpected VNC server message type {value}"),
                    ));
                }
            }
        }
    }

    fn read_framebuffer_rectangles(&mut self) -> Result<Vec<u8>, SessionError> {
        self.read_padding(1)?;
        let rects = self.read_u16()?;
        let mut rgb = vec![0; usize::from(self.width) * usize::from(self.height) * 3];
        for _ in 0..rects {
            let x = self.read_u16()?;
            let y = self.read_u16()?;
            let width = self.read_u16()?;
            let height = self.read_u16()?;
            let encoding = self.read_i32()?;
            if encoding != 0 {
                return Err(SessionError::new(
                    FailureReason::ScreenshotFailed,
                    format!("unsupported VNC framebuffer encoding {encoding}"),
                ));
            }
            self.read_raw_rectangle(&mut rgb, x, y, width, height)?;
        }
        Ok(rgb)
    }

    fn read_raw_rectangle(
        &mut self,
        rgb: &mut [u8],
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), SessionError> {
        let bytes_per_pixel = usize::from(self.pixel_format.bits_per_pixel / 8);
        let mut pixel = vec![0; bytes_per_pixel];
        for row in 0..height {
            for col in 0..width {
                self.stream.read_exact(&mut pixel).map_err(vnc_read_error)?;
                let (red, green, blue) = self.pixel_format.decode(&pixel);
                let dst_x = usize::from(x + col);
                let dst_y = usize::from(y + row);
                let dst = (dst_y * usize::from(self.width) + dst_x) * 3;
                if dst + 2 < rgb.len() {
                    rgb[dst] = red;
                    rgb[dst + 1] = green;
                    rgb[dst + 2] = blue;
                }
            }
        }
        Ok(())
    }

    fn pointer_event(&mut self, button_mask: u8, x: u16, y: u16) -> Result<(), SessionError> {
        let mut message = Vec::with_capacity(6);
        message.extend([5, button_mask]);
        message.extend(x.to_be_bytes());
        message.extend(y.to_be_bytes());
        self.stream
            .write_all(&message)
            .and_then(|()| self.stream.flush())
            .map_err(SessionError::internal)
    }

    fn read_u8(&mut self) -> Result<u8, SessionError> {
        let mut value = [0];
        self.stream.read_exact(&mut value).map_err(vnc_read_error)?;
        Ok(value[0])
    }

    fn read_u16(&mut self) -> Result<u16, SessionError> {
        let mut value = [0; 2];
        self.stream.read_exact(&mut value).map_err(vnc_read_error)?;
        Ok(u16::from_be_bytes(value))
    }

    fn read_i32(&mut self) -> Result<i32, SessionError> {
        let mut value = [0; 4];
        self.stream.read_exact(&mut value).map_err(vnc_read_error)?;
        Ok(i32::from_be_bytes(value))
    }

    fn read_u32(&mut self) -> Result<u32, SessionError> {
        let mut value = [0; 4];
        self.stream.read_exact(&mut value).map_err(vnc_read_error)?;
        Ok(u32::from_be_bytes(value))
    }

    fn read_padding(&mut self, bytes: usize) -> Result<(), SessionError> {
        let mut padding = vec![0; bytes];
        self.stream.read_exact(&mut padding).map_err(vnc_read_error)
    }
}

#[derive(Debug, Clone, Copy)]
struct VncPixelFormat {
    bits_per_pixel: u8,
    big_endian: bool,
    red_max: u16,
    green_max: u16,
    blue_max: u16,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
}

impl VncPixelFormat {
    fn parse(bytes: &[u8]) -> Result<Self, SessionError> {
        let bits_per_pixel = bytes[0];
        if !matches!(bits_per_pixel, 8 | 16 | 32) {
            return Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("unsupported VNC bits-per-pixel value {bits_per_pixel}"),
            ));
        }
        if bytes[3] == 0 {
            return Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                "unsupported VNC color-map pixel format",
            ));
        }

        Ok(Self {
            bits_per_pixel,
            big_endian: bytes[2] != 0,
            red_max: u16::from_be_bytes([bytes[4], bytes[5]]),
            green_max: u16::from_be_bytes([bytes[6], bytes[7]]),
            blue_max: u16::from_be_bytes([bytes[8], bytes[9]]),
            red_shift: bytes[10],
            green_shift: bytes[11],
            blue_shift: bytes[12],
        })
    }

    fn decode(&self, bytes: &[u8]) -> (u8, u8, u8) {
        let value = match (self.bits_per_pixel, self.big_endian) {
            (8, _) => u32::from(bytes[0]),
            (16, true) => u32::from(u16::from_be_bytes([bytes[0], bytes[1]])),
            (16, false) => u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            (32, true) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            (32, false) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            _ => 0,
        };

        (
            scale_color(value, self.red_shift, self.red_max),
            scale_color(value, self.green_shift, self.green_max),
            scale_color(value, self.blue_shift, self.blue_max),
        )
    }
}

fn scale_color(value: u32, shift: u8, max: u16) -> u8 {
    if max == 0 {
        return 0;
    }
    (((value >> shift) & u32::from(max)) * 255 / u32::from(max)) as u8
}

fn perform_vnc_security_handshake(
    stream: &mut TcpStream,
    version: &[u8; 12],
) -> Result<(), SessionError> {
    if version.starts_with(b"RFB 003.003") {
        let mut security_type = [0; 4];
        stream
            .read_exact(&mut security_type)
            .map_err(vnc_read_error)?;
        match u32::from_be_bytes(security_type) {
            1 => Ok(()),
            0 => Err(read_vnc_failure_reason(stream)),
            value => Err(SessionError::new(
                FailureReason::DisplayStartFailed,
                format!("unsupported VNC security type {value}"),
            )),
        }
    } else {
        let mut count = [0];
        stream.read_exact(&mut count).map_err(vnc_read_error)?;
        if count[0] == 0 {
            return Err(read_vnc_failure_reason(stream));
        }
        let mut security_types = vec![0; usize::from(count[0])];
        stream
            .read_exact(&mut security_types)
            .map_err(vnc_read_error)?;

        if security_types.contains(&RFB_SECURITY_TYPE_NONE) {
            stream
                .write_all(&[RFB_SECURITY_TYPE_NONE])
                .map_err(SessionError::internal)?;
            return read_vnc_security_result(stream);
        }

        if security_types.contains(&RFB_SECURITY_TYPE_APPLE_DH) {
            stream
                .write_all(&[RFB_SECURITY_TYPE_APPLE_DH])
                .map_err(SessionError::internal)?;
            perform_vnc_apple_dh_auth(stream)?;
            return read_vnc_security_result(stream);
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!(
                "VNC server did not offer a supported security type: {}",
                vnc_security_types_message(&security_types)
            ),
        ))
    }
}

const RFB_SECURITY_TYPE_NONE: u8 = 1;
const RFB_SECURITY_TYPE_APPLE_DH: u8 = 30;

fn read_vnc_security_result(stream: &mut TcpStream) -> Result<(), SessionError> {
    let mut result = [0; 4];
    stream.read_exact(&mut result).map_err(vnc_read_error)?;
    match u32::from_be_bytes(result) {
        0 => Ok(()),
        1 | 2 => Err(read_vnc_failure_reason(stream)),
        value => Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!("unknown VNC security result {value}"),
        )),
    }
}

fn perform_vnc_apple_dh_auth(stream: &mut TcpStream) -> Result<(), SessionError> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).map_err(vnc_read_error)?;
    let generator = u16::from_be_bytes([header[0], header[1]]);
    let key_len = usize::from(u16::from_be_bytes([header[2], header[3]]));
    if generator == 0 || key_len == 0 || key_len > 4096 {
        return Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!(
                "unsupported VNC Apple-DH parameters: generator={generator}, key_len={key_len}"
            ),
        ));
    }

    let mut modulus = vec![0; key_len];
    stream.read_exact(&mut modulus).map_err(vnc_read_error)?;
    let mut server_key = vec![0; key_len];
    stream.read_exact(&mut server_key).map_err(vnc_read_error)?;

    let (username, password) = vnc_credentials();
    let response = vnc_apple_dh_response(generator, &modulus, &server_key, &username, &password)?;
    stream.write_all(&response).map_err(SessionError::internal)
}

fn vnc_apple_dh_response(
    generator: u16,
    modulus: &[u8],
    server_key: &[u8],
    username: &str,
    password: &str,
) -> Result<Vec<u8>, SessionError> {
    let mut secret = vec![0; 512];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut secret))
        .map_err(SessionError::internal)?;
    vnc_apple_dh_response_with_secret(generator, modulus, server_key, username, password, &secret)
}

fn vnc_apple_dh_response_with_secret(
    generator: u16,
    modulus: &[u8],
    server_key: &[u8],
    username: &str,
    password: &str,
    secret: &[u8],
) -> Result<Vec<u8>, SessionError> {
    let modulus_value = BigUint::from_bytes_be(modulus);
    if modulus_value == BigUint::from(0u8) {
        return Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            "VNC Apple-DH server sent an empty modulus",
        ));
    }
    let secret_value = BigUint::from_bytes_be(secret);
    let generator_value = BigUint::from(generator);
    let server_key_value = BigUint::from_bytes_be(server_key);

    let public_key = generator_value.modpow(&secret_value, &modulus_value);
    let shared_key = server_key_value.modpow(&secret_value, &modulus_value);
    let shared = left_pad_bytes(&shared_key.to_bytes_be(), modulus.len())?;
    let digest = Md5::digest(&shared);

    let mut credentials = [0u8; 128];
    copy_padded_field(username.as_bytes(), &mut credentials[..64]);
    copy_padded_field(password.as_bytes(), &mut credentials[64..]);
    encrypt_aes128_ecb(&digest, &mut credentials)?;

    let mut response = credentials.to_vec();
    response.extend(left_pad_bytes(&public_key.to_bytes_be(), modulus.len())?);
    Ok(response)
}

fn encrypt_aes128_ecb(key: &[u8], data: &mut [u8]) -> Result<(), SessionError> {
    let cipher = Aes128::new_from_slice(key).map_err(SessionError::internal)?;
    for chunk in data.chunks_exact_mut(16) {
        let block = Block::<Aes128>::from_mut_slice(chunk);
        cipher.encrypt_block(block);
    }
    Ok(())
}

fn copy_padded_field(source: &[u8], target: &mut [u8]) {
    let len = source.len().min(target.len());
    target[..len].copy_from_slice(&source[..len]);
}

fn left_pad_bytes(bytes: &[u8], len: usize) -> Result<Vec<u8>, SessionError> {
    if bytes.len() > len {
        return Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            "VNC Apple-DH public key exceeded the server key length",
        ));
    }
    let mut padded = vec![0; len - bytes.len()];
    padded.extend(bytes);
    Ok(padded)
}

fn vnc_credentials() -> (String, String) {
    let username = std::env::var("FLATPAK_SMOKE_VNC_USERNAME")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("LOGNAME").ok())
        .unwrap_or_else(|| "root".to_string());
    let password =
        std::env::var("FLATPAK_SMOKE_VNC_PASSWORD").unwrap_or_else(|_| "flatpak-smoke".to_string());
    (username, password)
}

fn vnc_security_types_message(types: &[u8]) -> String {
    types
        .iter()
        .map(|security_type| match *security_type {
            1 => "none".to_string(),
            2 => "vnc-auth".to_string(),
            5 => "rsa-aes128".to_string(),
            19 => "vencrypt".to_string(),
            30 => "apple-dh".to_string(),
            129 => "rsa-aes256".to_string(),
            value => format!("unknown-{value}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_vnc_failure_reason(stream: &mut TcpStream) -> SessionError {
    let mut len = [0; 4];
    if stream.read_exact(&mut len).is_err() {
        return SessionError::new(FailureReason::DisplayStartFailed, "VNC handshake failed");
    }
    let mut reason = vec![0; u32::from_be_bytes(len) as usize];
    if stream.read_exact(&mut reason).is_err() {
        return SessionError::new(FailureReason::DisplayStartFailed, "VNC handshake failed");
    }
    SessionError::new(
        FailureReason::DisplayStartFailed,
        format!("VNC handshake failed: {}", String::from_utf8_lossy(&reason)),
    )
}

fn vnc_read_error(error: std::io::Error) -> SessionError {
    SessionError::new(
        FailureReason::ScreenshotFailed,
        format!("failed to read from Weston VNC server: {error}"),
    )
}

fn write_ppm(path: &Path, width: u16, height: u16, rgb: &[u8]) -> Result<(), SessionError> {
    let mut file = File::create(path).map_err(SessionError::internal)?;
    write!(file, "P6\n{width} {height}\n255\n").map_err(SessionError::internal)?;
    file.write_all(rgb).map_err(SessionError::internal)
}

pub(crate) fn click_screenshot_name(index: usize, click_text: &str) -> String {
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
    fn parses_imagemagick_absolute_error_metric() {
        assert_eq!(parse_absolute_error_metric("0"), Some(0));
        assert_eq!(parse_absolute_error_metric("120 (0.002)"), Some(120));
        assert_eq!(parse_absolute_error_metric("not a metric"), None);
    }

    #[test]
    fn rejects_cursor_only_screenshot_diffs() {
        assert!(!screenshot_diff_exceeds_threshold(
            776,
            SCREENSHOT_DIFF_PIXEL_THRESHOLD
        ));
        assert!(screenshot_diff_exceeds_threshold(
            4096,
            SCREENSHOT_DIFF_PIXEL_THRESHOLD
        ));
    }

    #[test]
    fn allows_small_post_click_screenshot_diffs() {
        assert!(!screenshot_diff_exceeds_threshold(
            25,
            POST_CLICK_SCREENSHOT_DIFF_PIXEL_THRESHOLD
        ));
        assert!(screenshot_diff_exceeds_threshold(
            26,
            POST_CLICK_SCREENSHOT_DIFF_PIXEL_THRESHOLD
        ));
    }

    #[test]
    fn parses_normalized_screenshot_standard_deviation() {
        assert_eq!(parse_standard_deviation("0"), Some(0.0));
        assert_eq!(parse_standard_deviation("0.278258"), Some(0.278258));
        assert_eq!(parse_standard_deviation("not a metric"), None);
    }

    #[test]
    fn builds_click_screenshot_names_from_labels() {
        assert_eq!(
            click_screenshot_name(1, "Log In"),
            "001-after-click-log-in.png"
        );
        assert_eq!(
            click_screenshot_name(12, "  ++  "),
            "012-after-click-text.png"
        );
    }

    #[test]
    fn finds_ocr_text_center_across_words_on_same_line() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t100\t50\t40\t20\t96\tLog\n",
            "5\t1\t1\t1\t1\t2\t148\t50\t24\t20\t96\tIn\n",
            "5\t1\t1\t1\t2\t1\t10\t90\t40\t20\t96\tOther\n",
        );

        assert_eq!(find_ocr_text_matches(tsv, "Log In"), vec![(136, 60)]);
        assert!(find_ocr_text_matches(tsv, "Sign In").is_empty());
    }

    #[test]
    fn rejects_ambiguous_ocr_text_matches_without_button_fallback() {
        let matches = [(120, 60), (120, 240)];
        let error = unambiguous_ocr_text_center(&matches, "Log In").unwrap_err();

        assert_eq!(error.reason, FailureReason::ScreenshotFailed);
        assert!(error.message.contains("matched multiple OCR locations"));
    }

    #[test]
    fn matches_button_crop_ocr_text_by_requested_label() {
        assert!(ocr_text_contains_label("q Log In >", "Log In"));
        assert!(ocr_text_contains_label(": Click Me :", "Click Me"));
        assert!(!ocr_text_contains_label("Advanced", "Log In"));
    }

    #[test]
    fn finds_single_primary_action_button_component() {
        let components = r#"
Objects (id: bounding-box centroid area mean-color):
  0: 1280x720+0+0 634.4,357.8 900926 srgb(0,0,0)
  5: 260x44+715+511 844.5,532.5 10796 srgb(255,255,255)
  2: 127x108+842+293 916.6,344.6 5252 srgb(255,255,255)
  1: 148x48+760+232 825.0,248.3 3032 srgb(255,255,255)
"#;

        assert_eq!(
            find_primary_action_button(components).map(ComponentBox::center),
            Some((845, 533))
        );
    }

    #[test]
    fn rejects_ambiguous_primary_action_button_components() {
        let components = r#"
  1: 260x44+715+511 844.5,532.5 10796 srgb(255,255,255)
  2: 240x40+700+590 820.0,610.0 9000 srgb(255,255,255)
"#;

        assert_eq!(find_primary_action_button(components), None);
    }

    #[test]
    fn decodes_little_endian_vnc_pixel_format() {
        let pixel_format =
            VncPixelFormat::parse(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])
                .unwrap();

        assert_eq!(
            pixel_format.decode(&[0x33, 0x22, 0x11, 0x00]),
            (0x11, 0x22, 0x33)
        );
    }

    #[test]
    fn builds_apple_dh_response_with_padded_public_key() {
        let response = vnc_apple_dh_response_with_secret(
            5,
            &[0x00, 0x17],
            &[0x00, 0x08],
            "root",
            "flatpak-smoke",
            &[0x03],
        )
        .unwrap();

        assert_eq!(response.len(), 130);
        assert_ne!(&response[..64], b"root");
        assert_eq!(&response[128..], &[0x00, 0x0a]);

        let mut expected_credentials = [0u8; 128];
        copy_padded_field(b"root", &mut expected_credentials[..64]);
        copy_padded_field(b"flatpak-smoke", &mut expected_credentials[64..]);
        encrypt_aes128_ecb(&Md5::digest([0x00, 0x06]), &mut expected_credentials).unwrap();
        assert_eq!(&response[..128], expected_credentials);

        let mut unpadded_credentials = [0u8; 128];
        copy_padded_field(b"root", &mut unpadded_credentials[..64]);
        copy_padded_field(b"flatpak-smoke", &mut unpadded_credentials[64..]);
        encrypt_aes128_ecb(&Md5::digest([0x06]), &mut unpadded_credentials).unwrap();
        assert_ne!(&response[..128], unpadded_credentials);
    }

    #[test]
    fn consumes_server_cut_text_padding_before_next_framebuffer_update() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(&[3, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c', 0, 0, 0, 0])
                .unwrap();
        });
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut client = VncClient {
            stream,
            width: 1,
            height: 1,
            pixel_format: VncPixelFormat {
                bits_per_pixel: 32,
                big_endian: false,
                red_max: 255,
                green_max: 255,
                blue_max: 255,
                red_shift: 16,
                green_shift: 8,
                blue_shift: 0,
            },
        };

        assert_eq!(client.read_framebuffer_update().unwrap(), vec![0, 0, 0]);
        server.join().unwrap();
    }

    #[test]
    fn labels_vnc_security_types_for_errors() {
        assert_eq!(
            vnc_security_types_message(&[129, 5, 30, 77]),
            "rsa-aes256, rsa-aes128, apple-dh, unknown-77"
        );
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
