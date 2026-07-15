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
    ) -> Result<SessionSuccess, SessionError> {
        let display_lease = DisplayLease::acquire()?;
        let display = display_lease.display();
        self.layout
            .append_runner_log(format!("starting Xvfb display {display}"))
            .map_err(SessionError::internal)?;

        let mut xvfb = self.spawn_xvfb(&display)?;
        let session_result = self.run_inside_display(app_ref, screenshot_name, &display, &mut xvfb);
        terminate_child(&mut xvfb);
        session_result
    }

    fn run_inside_display(
        &self,
        app_ref: &str,
        screenshot_name: &str,
        display: &str,
        xvfb: &mut Child,
    ) -> Result<SessionSuccess, SessionError> {
        self.wait_for_display(display, xvfb)?;
        let mut window_manager = self.spawn_openbox(display)?;
        let result = self.run_app_session(app_ref, screenshot_name, display);
        terminate_child(&mut window_manager);
        result
    }

    fn run_app_session(
        &self,
        app_ref: &str,
        screenshot_name: &str,
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

            let screenshot_path = self.layout.screenshot_path(screenshot_name);
            let relative_screenshot_path = self.layout.relative_screenshot_path(screenshot_name);
            let screenshotter =
                Screenshotter::new(display, self.env.clone(), self.layout.runner_log.clone());
            screenshotter.capture(
                &screenshot_path,
                self.bounded_timeout(self.screenshot_timeout)?,
            )?;
            if let Some(marker) = screenshotter.detect_app_error_text(&screenshot_path)? {
                return Err(SessionError::new(
                    FailureReason::AppErrorWindow,
                    format!("screenshot text matched app error marker '{marker}'"),
                )
                .with_screenshot(relative_screenshot_path));
            }

            Ok(SessionSuccess {
                screenshot_path: relative_screenshot_path,
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
    pub screenshot_path: String,
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

    fn with_screenshot(mut self, screenshot_path: String) -> Self {
        self.screenshots.push(screenshot_path);
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
