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
    session::{AppBackend, ensure_x11_socket_directory},
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
    backend: AppBackend,
    shell: Rc<RefCell<ManagedChild>>,
    app: Option<Rc<RefCell<ManagedChild>>>,
    keyring: Option<ManagedChild>,
    bus: String,
    app_id: String,
    version: String,
    helper_ready: Option<PathBuf>,
    x11: Option<X11Connection>,
}

struct X11Connection {
    display: String,
    authority: PathBuf,
}

impl X11Connection {
    fn from_helper(info: &Value, workspace: &Workspace) -> anyhow::Result<Self> {
        let display = info["x11"]["display"]
            .as_str()
            .context("GNOME did not report its X11 display")?;
        let number = display
            .strip_prefix(':')
            .context("GNOME reported a nonlocal X11 display")?;
        ensure!(
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()),
            "GNOME reported an invalid X11 display"
        );
        let authority = PathBuf::from(
            info["x11"]["authority"]
                .as_str()
                .context("GNOME did not report its Xauthority file")?,
        );
        ensure!(
            authority.is_file()
                && authority
                    .canonicalize()?
                    .starts_with(workspace.home.join("runtime").canonicalize()?),
            "GNOME Xauthority file is outside the private runtime directory"
        );
        Ok(Self {
            display: display.to_string(),
            authority,
        })
    }

    fn apply(&self, command: &mut std::process::Command) {
        command
            .env("DISPLAY", &self.display)
            .env("XAUTHORITY", &self.authority);
    }
}

impl Gnome {
    pub fn start(
        workspace: Rc<Workspace>,
        layout: &OutputLayout,
        backend: AppBackend,
        deadline: Instant,
    ) -> anyhow::Result<Self> {
        if backend == AppBackend::Xwayland {
            // Mutter reserves both public and service displays. Its first mkdir
            // obeys umask; its second pass rejects the resulting non-1777 mode.
            ensure_x11_socket_directory().context("preparing Xwayland socket directory")?;
        }
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
        let shell_args = match backend {
            AppBackend::Wayland => {
                "printf %s \"$DBUS_SESSION_BUS_ADDRESS\" > \"$1\"; exec gnome-shell --headless --wayland --virtual-monitor=1280x900 --no-x11"
            }
            AppBackend::Xwayland => {
                "printf %s \"$DBUS_SESSION_BUS_ADDRESS\" > \"$1\"; exec gnome-shell --headless --wayland --virtual-monitor=1280x900"
            }
        };
        let shell = workspace
            .command("dbus-run-session")
            .args(["--", "sh", "-c", shell_args, "capture-session"])
            .arg(&bus_path)
            .env(
                "FLATPAK_SMOKE_HELPER_HEALTH_FILE",
                workspace.home.join("helper-ready"),
            )
            .stdout(File::create(layout.logs_dir.join("desktop.stdout.log"))?)
            .stderr(File::create(layout.logs_dir.join("desktop.stderr.log"))?)
            .spawn_managed()
            .context("starting GNOME screenshot session")?;
        layout.append_runner_log(format!("desktop launcher pid: {}", shell.id()))?;
        let mut desktop = Self {
            workspace,
            backend,
            shell: Rc::new(RefCell::new(shell)),
            app: None,
            keyring: None,
            bus: String::new(),
            app_id: String::new(),
            version: String::new(),
            helper_ready: None,
            x11: None,
        };
        let mut last_error = String::new();
        loop {
            desktop.check(deadline)?;
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
                        desktop.check(deadline)?;
                        if backend == AppBackend::Xwayland {
                            let connection = X11Connection::from_helper(&info, &desktop.workspace)?;
                            let mut probe = desktop.workspace.command("xdpyinfo");
                            connection.apply(&mut probe);
                            let x11 = probe
                                .output_before_checked(deadline, &desktop.health())
                                .context("checking the GNOME Xwayland display")?;
                            ensure!(
                                x11.status.success(),
                                "GNOME Xwayland display {} is not ready: {}",
                                connection.display,
                                String::from_utf8_lossy(&x11.stderr).trim()
                            );
                            desktop.x11 = Some(connection);
                        }
                        return Ok(desktop);
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            if sleep_before_checked(Duration::from_millis(100), deadline, &desktop.health())
                .is_err()
            {
                desktop.check(deadline)?;
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
        args: &[String],
        layout: &OutputLayout,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        self.app_id = app_ref.id().to_string();
        let password = self.workspace.home.join("keyring-password");
        fs::write(&password, b"flatpak-smoke-capture")?;
        let keyring = self
            .workspace
            .command("gnome-keyring-daemon")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .args([
                "--foreground",
                "--unlock",
                "--components=secrets",
                "--control-directory",
            ])
            .arg(self.workspace.home.join("runtime/keyring"))
            .stdin(File::open(&password)?)
            .stdout(File::create(layout.logs_dir.join("keyring.stdout.log"))?)
            .stderr(File::create(layout.logs_dir.join("keyring.stderr.log"))?)
            .spawn_managed()?;
        self.keyring = Some(keyring);
        // One foreground daemon unlocks this disposable profile. Starting a
        // second --unlock process can race initialization and corrupt login.keyring.
        loop {
            if let Some(status) = self.keyring.as_mut().unwrap().try_wait()? {
                anyhow::bail!("session keyring exited with {status}");
            }
            if self.call(json!({"action": "keyring_ready"}), deadline)? == true {
                break;
            }
            sleep_before_checked(Duration::from_millis(50), deadline, &self.health())
                .context("waiting for an unlocked session keyring")?;
        }
        let display_args: &[&str] = match self.backend {
            AppBackend::Wayland => &[
                "--env=GDK_BACKEND=wayland",
                "--env=QT_QPA_PLATFORM=wayland",
                "--env=SDL_VIDEODRIVER=wayland",
                "--env=MOZ_ENABLE_WAYLAND=1",
            ],
            AppBackend::Xwayland => &[
                "--env=GDK_BACKEND=x11",
                "--env=QT_QPA_PLATFORM=xcb",
                "--env=SDL_VIDEODRIVER=x11",
                "--env=MOZ_ENABLE_WAYLAND=0",
            ],
        };
        let mut command = self.workspace.command("flatpak");
        if let Some(connection) = &self.x11 {
            // Let Flatpak map the authenticated session socket into its sandbox.
            // A sandbox --env=DISPLAY override alone cannot provide that socket.
            connection.apply(&mut command);
        }
        let app = command
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .args(["run", "--user"])
            .arg(format!("--arch={}", app_ref.arch()))
            .arg(format!("--branch={}", app_ref.branch()))
            .args(display_args)
            .args([
                "--env=GTK_A11Y=atspi",
                "--env=QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1",
            ])
            .arg(app_ref.id())
            .args(args)
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
        self.check(deadline)?;
        let millis = remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128);
        let output = self
            .workspace
            .command("python3")
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .arg(self.workspace.home.join("client.py"))
            .arg(serde_json::to_string(&request)?)
            .arg(millis.to_string())
            .output_before_checked(deadline, &self.health());
        self.check(deadline)?;
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
    workspace: &Workspace,
    app_id: &str,
    deadline: Instant,
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
        // Some launchers (including VSCodium) exit after starting another
        // Flatpak instance. ps is scoped to this workspace's XDG_RUNTIME_DIR.
        let running = if status.success() {
            let instances = workspace
                .command("flatpak")
                .args(["ps", "--columns=application"])
                .output_before_checked(
                    deadline.min(Instant::now() + Duration::from_secs(1)),
                    &HealthCheck::default(),
                )?;
            ensure!(
                instances.status.success(),
                "checking screenshot app instances"
            );
            String::from_utf8_lossy(&instances.stdout)
                .lines()
                .any(|id| id.trim() == app_id)
        } else {
            false
        };
        if !running {
            return Err(failure(
                FailureReason::EarlyExit,
                format!("app exited during capture with {status}"),
            ));
        }
    }
    if let Some(path) = helper_ready
        && !path.is_file()
    {
        return Err(failure(
            FailureReason::ScreenshotFailed,
            "native capture helper disconnected",
        ));
    }
    remaining(deadline)?;
    Ok(())
}

impl Desktop for Gnome {
    fn version(&self) -> &str {
        &self.version
    }
    fn check(&self, deadline: Instant) -> anyhow::Result<()> {
        check_processes(
            &self.shell,
            self.app.as_deref(),
            self.helper_ready.as_deref(),
            &self.workspace,
            &self.app_id,
            deadline,
        )
    }
    fn health(&self) -> HealthCheck {
        let shell = self.shell.clone();
        let app = self.app.clone();
        let helper_ready = self.helper_ready.clone();
        let workspace = self.workspace.clone();
        let app_id = self.app_id.clone();
        HealthCheck::new(move |deadline| {
            check_processes(
                &shell,
                app.as_deref(),
                helper_ready.as_deref(),
                &workspace,
                &app_id,
                deadline,
            )
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
            // Also stop detached/re-executed instances that left the launcher's
            // process group. The private runtime directory excludes other runs.
            if let Ok(mut cleanup) = self
                .workspace
                .command("flatpak")
                .args(["kill", &self.app_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn_managed()
            {
                // Cleanup must run even when remaining() reports cancellation.
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline && matches!(cleanup.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            app.borrow_mut().terminate();
        }
        if let Some(keyring) = &mut self.keyring {
            keyring.terminate();
        }
        if self.backend == AppBackend::Xwayland && self.helper_ready.is_some() {
            // Killing the whole group at once can kill mandatory Xwayland before
            // Mutter runs prepare-shutdown and removes its sockets/locks. Ask the
            // owning compositor to quit first, then retain forced group cleanup.
            // Like Flatpak instance cleanup, this must also run after cancellation.
            let deadline = Instant::now() + Duration::from_secs(1);
            if let Ok(mut request) = self
                .workspace
                .command("python3")
                .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
                .arg(self.workspace.home.join("client.py"))
                .arg(r#"{"action":"quit"}"#)
                .arg("500")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn_managed()
            {
                while Instant::now() < deadline && matches!(request.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                while Instant::now() < deadline
                    && matches!(self.shell.borrow_mut().try_wait(), Ok(None))
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        self.shell.borrow_mut().terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_connection_uses_reported_display_and_private_authority() {
        let workspace = Workspace::prepare().unwrap();
        let authority = workspace.home.join("runtime/test-Xauthority");
        fs::write(&authority, b"test cookie").unwrap();
        let info = json!({"x11": {"display": ":7", "authority": authority}});
        let connection = X11Connection::from_helper(&info, &workspace).unwrap();
        let mut command = workspace.command("unused");
        connection.apply(&mut command);
        let env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("DISPLAY")],
            Some(std::ffi::OsStr::new(":7"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new("XAUTHORITY")],
            Some(authority.as_os_str())
        );
    }

    #[test]
    fn x11_connection_rejects_missing_remote_and_foreign_credentials() {
        let workspace = Workspace::prepare().unwrap();
        let authority = workspace.home.join("runtime/test-Xauthority");
        fs::write(&authority, b"test cookie").unwrap();
        for display in ["", ":", ":0 trailing", "host:0", ":-1"] {
            assert!(
                X11Connection::from_helper(
                    &json!({"x11": {"display": display, "authority": authority}}),
                    &workspace
                )
                .is_err()
            );
        }
        assert!(X11Connection::from_helper(&json!({}), &workspace).is_err());
        let foreign = tempfile::NamedTempFile::new().unwrap();
        assert!(
            X11Connection::from_helper(
                &json!({"x11": {"display": ":0", "authority": foreign.path()}}),
                &workspace
            )
            .is_err()
        );
        let link = workspace.home.join("runtime/foreign-link");
        std::os::unix::fs::symlink(foreign.path(), &link).unwrap();
        assert!(
            X11Connection::from_helper(
                &json!({"x11": {"display": ":0", "authority": link}}),
                &workspace
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires GNOME screenshot image, system bus and a headless container"]
    fn xwayland_session_starts_with_authenticated_readiness() {
        let temp = tempfile::tempdir().unwrap();
        let layout = OutputLayout::prepare(&temp.path().join("output"), false).unwrap();
        let workspace = Rc::new(Workspace::prepare().unwrap());
        let desktop = Gnome::start(
            workspace,
            &layout,
            AppBackend::Xwayland,
            Instant::now() + Duration::from_secs(20),
        );
        if let Err(error) = &desktop {
            eprintln!(
                "{}",
                fs::read_to_string(layout.logs_dir.join("desktop.stderr.log")).unwrap_or_default()
            );
            panic!("Xwayland capture startup failed: {error:#}");
        }
        let first = desktop.unwrap();
        // A second compositor must discover its own display/cookie rather than
        // connecting to the first session's :0 or inheriting caller credentials.
        let second_workspace = Rc::new(Workspace::prepare().unwrap());
        let second_layout =
            OutputLayout::prepare(&temp.path().join("second-output"), false).unwrap();
        let second = Gnome::start(
            second_workspace,
            &second_layout,
            AppBackend::Xwayland,
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap();
        assert_ne!(
            first.x11.as_ref().unwrap().display,
            second.x11.as_ref().unwrap().display
        );
        assert_ne!(
            first.x11.as_ref().unwrap().authority,
            second.x11.as_ref().unwrap().authority
        );
        first
            .check(Instant::now() + Duration::from_secs(1))
            .unwrap();
        second
            .check(Instant::now() + Duration::from_secs(1))
            .unwrap();
    }
}
