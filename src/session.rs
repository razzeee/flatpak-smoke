use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
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
    ) -> Result<SessionSuccess, SessionError> {
        let display = WAYLAND_DISPLAY.to_string();
        self.layout
            .append_runner_log(format!("starting Weston Wayland display {display}"))
            .map_err(SessionError::internal)?;

        let mut weston = self.start_weston(&display)?;
        let session_result = self.run_app_session(app_ref, screenshot_name, &display);
        terminate_child(&mut weston);
        session_result
    }

    fn run_app_session(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        display: &str,
    ) -> Result<SessionSuccess, SessionError> {
        let launch_started = Instant::now();
        let screenshotter =
            Screenshotter::new(display, self.env.clone(), self.layout.runner_log.clone());
        let baseline_path = self.layout.logs_dir.join("wayland-baseline.png");
        screenshotter.capture(
            &baseline_path,
            self.bounded_timeout(self.screenshot_timeout)?,
        )?;

        let mut app = self.spawn_app(app_ref, display)?;
        let result = (|| {
            self.wait_for_app_frame(
                &mut app,
                &screenshotter,
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

            let screenshot_path = self.layout.screenshot_path(screenshot_name);
            let relative_screenshot_path = self.layout.relative_screenshot_path(screenshot_name);
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

            Ok(SessionSuccess {
                screenshot_paths: screenshots,
                launch_to_window_ms: launch_to_window,
            })
        })();

        terminate_child(&mut app);
        terminate_keyring_unlock_daemons(&self.env);
        result
    }

    fn start_weston(&self, display: &str) -> Result<Child, SessionError> {
        let mut last_error = None;
        for backend in ["headless", "headless-backend.so"] {
            let mut weston = self.spawn_weston(display, backend)?;
            match self.wait_for_compositor(display, &mut weston) {
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

    fn spawn_weston(&self, display: &str, backend: &str) -> Result<Child, SessionError> {
        let stdout = File::create(self.layout.logs_dir.join("weston.stdout.log"))
            .map_err(SessionError::internal)?;
        let stderr = File::create(self.layout.logs_dir.join("weston.stderr.log"))
            .map_err(SessionError::internal)?;
        self.base_command("weston", display)
            .arg(format!("--backend={backend}"))
            .arg(format!("--socket={display}"))
            .args(["--width=1280", "--height=720", "--idle-time=0", "--debug"])
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

    fn wait_for_compositor(&self, display: &str, weston: &mut Child) -> Result<(), SessionError> {
        let timeout = self.bounded_timeout(self.display_timeout)?;
        let started = Instant::now();
        let screenshotter =
            Screenshotter::new(display, self.env.clone(), self.layout.runner_log.clone());
        let readiness_path = self.layout.logs_dir.join("wayland-readiness.png");
        while started.elapsed() < timeout {
            if let Some(status) = weston.try_wait().map_err(SessionError::internal)? {
                return Err(SessionError::new(
                    FailureReason::DisplayStartFailed,
                    format!("Weston exited before the Wayland display became ready with {status}"),
                ));
            }

            if screenshotter.capture_once(&readiness_path).is_ok() {
                return match weston.try_wait().map_err(SessionError::internal)? {
                    Some(status) => Err(SessionError::new(
                        FailureReason::DisplayStartFailed,
                        format!("Weston exited after display readiness check with {status}"),
                    )),
                    None => Ok(()),
                };
            }

            thread::sleep(Duration::from_millis(100));
        }

        Err(SessionError::new(
            FailureReason::DisplayStartFailed,
            format!(
                "Weston Wayland display did not become ready within {}s",
                timeout.as_secs()
            ),
        ))
    }

    fn wait_for_app_frame(
        &self,
        app: &mut Child,
        screenshotter: &Screenshotter,
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

            screenshotter.capture_once(&candidate_path)?;
            if screenshotter.screenshots_differ(baseline_path, &candidate_path)? {
                self.layout
                    .append_runner_log("visible Wayland frame detected from screenshot delta")
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

    fn capture(&self, path: &Path, timeout: Duration) -> Result<(), SessionError> {
        let started = Instant::now();
        let mut last_error = None;

        while started.elapsed() < timeout {
            match self.capture_once(path) {
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
        let temp = tempfile::tempdir().map_err(SessionError::internal)?;
        let output = self
            .command("weston-screenshooter")
            .current_dir(temp.path())
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let captured = find_screenshot_file(temp.path())?;
                fs::copy(&captured, path).map_err(SessionError::internal)?;
                ensure_file_nonempty(path).map_err(SessionError::internal)?;
                self.append_log(format!("screenshot captured at '{}'", path.display()))?;
                Ok(())
            }
            Ok(output) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!(
                    "failed to capture Wayland screenshot with weston-screenshooter: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(SessionError::new(
                FailureReason::DependencyFailed,
                "missing required tool: weston-screenshooter",
            )),
            Err(error) => Err(SessionError::new(
                FailureReason::ScreenshotFailed,
                format!("failed to run weston-screenshooter: {error}"),
            )),
        }
    }

    fn screenshots_differ(&self, baseline: &Path, candidate: &Path) -> Result<bool, SessionError> {
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
                    .map(|different_pixels| different_pixels > SCREENSHOT_DIFF_PIXEL_THRESHOLD)
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

const SCREENSHOT_DIFF_PIXEL_THRESHOLD: u64 = 25;

fn find_screenshot_file(dir: &Path) -> Result<PathBuf, SessionError> {
    let mut png_files = Vec::new();
    let mut other_files = Vec::new();
    for entry in fs::read_dir(dir).map_err(SessionError::internal)? {
        let entry = entry.map_err(SessionError::internal)?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(SessionError::internal)?;
        if !metadata.is_file() || metadata.len() == 0 {
            continue;
        }

        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        {
            png_files.push(path);
        } else {
            other_files.push(path);
        }
    }

    png_files.sort();
    other_files.sort();
    png_files
        .into_iter()
        .chain(other_files)
        .next()
        .ok_or_else(|| {
            SessionError::new(
                FailureReason::ScreenshotFailed,
                "weston-screenshooter completed but produced no screenshot file",
            )
        })
}

fn parse_absolute_error_metric(output: &str) -> Option<u64> {
    output
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
        .map(|value| value.round() as u64)
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
    fn finds_nonempty_weston_screenshot_file() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("empty.png"), "").unwrap();
        fs::write(temp.path().join("wayland-screenshot.png"), "png").unwrap();

        assert_eq!(
            find_screenshot_file(temp.path()).unwrap(),
            temp.path().join("wayland-screenshot.png")
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
