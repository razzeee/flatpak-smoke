use super::{
    app_ref::AppRef,
    desktop::{Desktop, Input, Window},
    failure,
    workspace::Workspace,
};
use crate::{
    output::OutputLayout,
    process::{BoundedCommand, HealthCheck, ManagedChild, remaining, sleep_before_checked},
    result::FailureReason,
};
use anyhow::{Context, ensure};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Stdio,
    rc::Rc,
    time::{Duration, Instant},
};

pub const METADATA: &str = include_str!("../../desktop/gnome/metadata.json");
pub const EXTENSION: &str = include_str!("../../desktop/gnome/extension.js");
pub const CLIENT: &str = include_str!("../../desktop/gnome/client.py");

pub struct Gnome {
    pub workspace: Rc<Workspace>,
    shell: Rc<RefCell<ManagedChild>>,
    app: Option<Rc<RefCell<ManagedChild>>>,
    keyring: Option<ManagedChild>,
    bus: String,
    app_id: String,
    version: String,
    helper_ready: Option<PathBuf>,
}

impl Gnome {
    pub fn start(
        workspace: Rc<Workspace>,
        layout: &OutputLayout,
        deadline: Instant,
    ) -> anyhow::Result<Self> {
        let extension = workspace
            .home
            .join("data/gnome-shell/extensions/capture@flatpak-smoke");
        fs::create_dir_all(&extension)?;
        fs::write(extension.join("metadata.json"), METADATA)?;
        fs::write(extension.join("extension.js"), EXTENSION)?;
        fs::write(workspace.home.join("client.py"), CLIENT)?;
        let output = workspace
            .command("gsettings")
            .args([
                "set",
                "org.gnome.shell",
                "enabled-extensions",
                "['capture@flatpak-smoke']",
            ])
            .output_before_checked(deadline, &HealthCheck::default())?;
        ensure!(
            output.status.success(),
            "setting GNOME capture extension: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = workspace
            .command("gsettings")
            .args([
                "set",
                "org.gnome.desktop.interface",
                "toolkit-accessibility",
                "true",
            ])
            .output_before_checked(deadline, &HealthCheck::default())?;
        ensure!(output.status.success(), "enabling session accessibility");
        let bus_path = workspace.home.join("session-bus");
        let shell = workspace.command("dbus-run-session").args(["--", "sh", "-c",
            "printf %s \"$DBUS_SESSION_BUS_ADDRESS\" > \"$1\"; exec gnome-shell --headless --wayland --virtual-monitor=1280x900 --no-x11", "capture-session"])
            .arg(&bus_path)
            .env("FLATPAK_SMOKE_HELPER_HEALTH_FILE", workspace.home.join("helper-ready"))
            .stdout(File::create(layout.logs_dir.join("desktop.stdout.log"))?)
            .stderr(File::create(layout.logs_dir.join("desktop.stderr.log"))?)
            .spawn_managed().context("starting GNOME screenshot session")?;
        layout.append_runner_log(format!("desktop launcher pid: {}", shell.id()))?;
        let mut desktop = Self {
            workspace,
            shell: Rc::new(RefCell::new(shell)),
            app: None,
            keyring: None,
            bus: String::new(),
            app_id: String::new(),
            version: String::new(),
            helper_ready: None,
        };
        let mut last_error = String::new();
        loop {
            desktop.check()?;
            if let Ok(bus) = fs::read_to_string(&bus_path) {
                desktop.bus = bus;
            }
            if !desktop.bus.is_empty() {
                match desktop.call(
                    json!({"action": "ping"}),
                    deadline.min(Instant::now() + Duration::from_secs(1)),
                ) {
                    Ok(info) => {
                        ensure!(info["protocol"] == 1, "unsupported desktop helper protocol");
                        desktop.version = info["version"]
                            .as_str()
                            .context("desktop did not report version")?
                            .to_string();
                        desktop.helper_ready = Some(desktop.workspace.home.join("helper-ready"));
                        desktop.check()?;
                        return Ok(desktop);
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            if sleep_before_checked(Duration::from_millis(100), deadline, &desktop.health())
                .is_err()
            {
                desktop.check()?;
                return Err(failure(
                    FailureReason::DisplayStartFailed,
                    format!("GNOME helper did not become ready: {last_error}"),
                ));
            }
        }
    }

    pub fn launch(
        &mut self,
        app_ref: &AppRef,
        layout: &OutputLayout,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        self.app_id = app_ref.id().to_string();
        let keyring = self
            .workspace
            .command("gnome-keyring-daemon")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .args([
                "--foreground",
                "--components=secrets",
                "--control-directory",
            ])
            .arg(self.workspace.home.join("runtime/keyring"))
            .stdout(File::create(layout.logs_dir.join("keyring.stdout.log"))?)
            .stderr(File::create(layout.logs_dir.join("keyring.stderr.log"))?)
            .spawn_managed()?;
        self.keyring = Some(keyring);
        let unlocked = self.workspace.command("sh")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .args(["-c", "printf '\\n' | gnome-keyring-daemon --unlock --components=secrets --control-directory \"$XDG_RUNTIME_DIR/keyring\""])
            .output_before_checked(deadline, &self.health())?;
        ensure!(
            unlocked.status.success(),
            "unlocking session keyring: {}",
            String::from_utf8_lossy(&unlocked.stderr)
        );
        let app = self
            .workspace
            .command("flatpak")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .args(["run", "--user"])
            .arg(format!("--arch={}", app_ref.arch()))
            .arg(format!("--branch={}", app_ref.branch()))
            .args([
                "--env=GDK_BACKEND=wayland",
                "--env=QT_QPA_PLATFORM=wayland",
                "--env=SDL_VIDEODRIVER=wayland",
                "--env=MOZ_ENABLE_WAYLAND=1",
                "--env=GTK_A11Y=atspi",
                "--env=QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1",
            ])
            .arg(app_ref.id())
            .stdin(Stdio::null())
            .stdout(File::create(&layout.app_stdout)?)
            .stderr(File::create(&layout.app_stderr)?)
            .spawn_managed()
            .context("launching screenshot application")?;
        layout.append_runner_log(format!("app launcher pid: {}", app.id()))?;
        self.app = Some(Rc::new(RefCell::new(app)));
        Ok(())
    }

    fn call(&self, request: Value, deadline: Instant) -> anyhow::Result<Value> {
        self.check()?;
        let millis = remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128);
        let output = self
            .workspace
            .command("python3")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .arg(self.workspace.home.join("client.py"))
            .arg(serde_json::to_string(&request)?)
            .arg(millis.to_string())
            .output_before_checked(deadline, &self.health());
        self.check()?;
        let output = output?;
        ensure!(
            output.status.success(),
            "desktop helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let response: Value =
            serde_json::from_slice(&output.stdout).context("invalid desktop helper response")?;
        ensure!(
            response["ok"] == true,
            "desktop helper: {}",
            response["error"].as_str().unwrap_or("unknown error")
        );
        Ok(response["result"].clone())
    }

    fn window_call(&self, id: u64, mut request: Value, deadline: Instant) -> anyhow::Result<Value> {
        request["window"] = json!(id);
        request["app_id"] = json!(self.app_id);
        self.call(request, deadline)
    }
}

fn check_processes(
    shell: &RefCell<ManagedChild>,
    app: Option<&RefCell<ManagedChild>>,
    helper_ready: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(status) = shell.borrow_mut().try_wait()? {
        return Err(failure(
            FailureReason::DisplayExited,
            format!("GNOME exited during capture with {status}"),
        ));
    }
    if let Some(app) = app
        && let Some(status) = app.borrow_mut().try_wait()?
    {
        return Err(failure(
            FailureReason::EarlyExit,
            format!("app exited during capture with {status}"),
        ));
    }
    if let Some(path) = helper_ready
        && !path.is_file()
    {
        return Err(failure(
            FailureReason::ScreenshotFailed,
            "native capture helper disconnected",
        ));
    }
    Ok(())
}

impl Desktop for Gnome {
    fn version(&self) -> &str {
        &self.version
    }
    fn check(&self) -> anyhow::Result<()> {
        check_processes(
            &self.shell,
            self.app.as_deref(),
            self.helper_ready.as_deref(),
        )
    }
    fn health(&self) -> HealthCheck {
        let shell = self.shell.clone();
        let app = self.app.clone();
        let helper_ready = self.helper_ready.clone();
        HealthCheck::new(move || {
            check_processes(&shell, app.as_deref(), helper_ready.as_deref())
                .map_err(std::io::Error::other)
        })
    }
    fn windows(&self, deadline: Instant) -> anyhow::Result<Vec<Window>> {
        let windows: Vec<Window> =
            serde_json::from_value(self.call(json!({"action": "windows"}), deadline)?)?;
        Ok(windows
            .into_iter()
            .filter(|window| window.app_id == self.app_id)
            .collect())
    }
    fn resize(
        &self,
        window: u64,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> anyhow::Result<Window> {
        Ok(serde_json::from_value(self.window_call(
            window,
            json!({"action": "resize", "width": width, "height": height}),
            deadline,
        )?)?)
    }
    fn capture(&self, window: u64, path: &Path, deadline: Instant) -> anyhow::Result<Window> {
        let path = path
            .parent()
            .context("capture path has no parent")?
            .canonicalize()?
            .join(path.file_name().context("capture path has no filename")?);
        Ok(serde_json::from_value(self.window_call(
            window,
            json!({"action": "capture", "path": path}),
            deadline,
        )?)?)
    }
    fn input(&self, window: u64, input: Input<'_>, deadline: Instant) -> anyhow::Result<()> {
        let request = match input {
            Input::Click(x, y) => json!({"action": "click", "x": x, "y": y}),
            Input::Key(keys) => json!({"action": "key", "keys": keys}),
            Input::Text(text) => json!({"action": "type_text", "text": text}),
        };
        self.window_call(window, request, deadline)?;
        Ok(())
    }
}

impl Drop for Gnome {
    fn drop(&mut self) {
        if let Some(app) = &self.app {
            app.borrow_mut().terminate();
        }
        if let Some(keyring) = &mut self.keyring {
            keyring.terminate();
        }
        self.shell.borrow_mut().terminate();
    }
}
