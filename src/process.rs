use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    net::TcpStream,
    ops::{Deref, DerefMut},
    os::unix::process::CommandExt,
    process::{Child, Command, Output, Stdio},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

static CANCELLED: AtomicBool = AtomicBool::new(false);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLEANUP_GRACE: Duration = Duration::from_millis(200);

/// Run-local check sharing the caller's absolute deadline, including any I/O.
#[derive(Clone, Default)]
pub struct HealthCheck(Option<Rc<dyn Fn(Instant) -> io::Result<()>>>);

impl HealthCheck {
    pub fn new(check: impl Fn(Instant) -> io::Result<()> + 'static) -> Self {
        Self(Some(Rc::new(check)))
    }

    pub fn check(&self, deadline: Instant) -> io::Result<()> {
        self.0.as_ref().map_or(Ok(()), |check| check(deadline))
    }
}

extern "C" fn cancel(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

pub fn install_signal_handlers() -> io::Result<()> {
    // The handler only sets an atomic flag. Cleanup runs on the normal execution path.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = cancel as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

pub fn remaining(deadline: Instant) -> io::Result<Duration> {
    if CANCELLED.load(Ordering::Relaxed) {
        return Err(io::Error::other("run cancelled"));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline elapsed"));
    }
    Ok(remaining)
}

pub fn sleep_before(duration: Duration, deadline: Instant) -> io::Result<()> {
    sleep_before_checked(duration, deadline, &HealthCheck::default())
}

pub fn sleep_before_checked(
    duration: Duration,
    deadline: Instant,
    health: &HealthCheck,
) -> io::Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        health.check(deadline)?;
        let budget = remaining(deadline)?;
        thread::sleep(
            budget
                .min(POLL_INTERVAL)
                .min(until.saturating_duration_since(Instant::now())),
        );
    }
    health.check(deadline)?;
    remaining(deadline).map(|_| ())
}

/// Owns a separate process group, including descendants left behind by its leader.
pub struct ManagedChild {
    child: Child,
    group: libc::pid_t,
}

impl Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl ManagedChild {
    pub fn terminate(&mut self) {
        if self.group == 0 {
            return;
        }
        // Negative PIDs address only the process group created for this child.
        unsafe {
            libc::kill(-self.group, libc::SIGTERM);
        }
        let deadline = Instant::now() + CLEANUP_GRACE;
        while Instant::now() < deadline {
            let _ = self.child.try_wait();
            if unsafe { libc::kill(-self.group, 0) } == -1 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        let _ = self.child.wait();
        self.group = 0;
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub trait BoundedCommand {
    fn spawn_managed(&mut self) -> io::Result<ManagedChild>;
    #[cfg(test)]
    fn output_before(&mut self, deadline: Instant) -> io::Result<Output>;
    fn output_before_checked(
        &mut self,
        deadline: Instant,
        health: &HealthCheck,
    ) -> io::Result<Output>;
}

impl BoundedCommand for Command {
    fn spawn_managed(&mut self) -> io::Result<ManagedChild> {
        self.process_group(0);
        let child = self.spawn()?;
        let group = child.id() as libc::pid_t;
        Ok(ManagedChild { child, group })
    }

    #[cfg(test)]
    fn output_before(&mut self, deadline: Instant) -> io::Result<Output> {
        self.output_before_checked(deadline, &HealthCheck::default())
    }

    fn output_before_checked(
        &mut self,
        deadline: Instant,
        health: &HealthCheck,
    ) -> io::Result<Output> {
        health.check(deadline)?;
        remaining(deadline)?;
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut child = self
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn_managed()?;
        let status = loop {
            health.check(deadline)?;
            remaining(deadline)?;
            if let Some(status) = child.try_wait()? {
                break status;
            }
            sleep_before_checked(POLL_INTERVAL, deadline, health)?;
        };
        child.terminate();
        stdout.seek(SeekFrom::Start(0))?;
        stderr.seek(SeekFrom::Start(0))?;
        let mut output = Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        stdout.read_to_end(&mut output.stdout)?;
        stderr.read_to_end(&mut output.stderr)?;
        health.check(deadline)?;
        remaining(deadline)?;
        Ok(output)
    }
}

/// Rechecks the absolute deadline between partial reads, including read_exact calls.
pub struct DeadlineStream {
    stream: TcpStream,
    pub deadline: Instant,
    pub health: HealthCheck,
}

impl DeadlineStream {
    pub fn new(stream: TcpStream, deadline: Instant) -> Self {
        Self {
            stream,
            deadline,
            health: HealthCheck::default(),
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            self.health.check(self.deadline)?;
            self.stream
                .set_read_timeout(Some(remaining(self.deadline)?.min(POLL_INTERVAL)))?;
            match self.stream.read(buf) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            self.health.check(self.deadline)?;
            self.stream
                .set_write_timeout(Some(remaining(self.deadline)?.min(POLL_INTERVAL)))?;
            match self.stream.write(buf) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.health.check(self.deadline)?;
        remaining(self.deadline)?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, net::TcpListener, path::Path};

    fn assert_process_stopped(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
            // Orphans can remain zombies until the host's PID 1 reaps them.
            if stat
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                || stat.as_ref().is_ok_and(|value| {
                    value
                        .rsplit_once(") ")
                        .is_some_and(|(_, fields)| fields.starts_with('Z'))
                })
            {
                return;
            }
            assert!(Instant::now() < deadline, "process {pid} survived cleanup");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn read_pid(path: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "helper did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn timeout_kills_term_ignoring_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let started = Instant::now();
        let error = Command::new("sh")
            .args([
                "-c",
                "trap '' TERM; sleep 30 & echo $! > \"$1\"; wait",
                "test",
            ])
            .arg(&pid_file)
            .output_before(started + Duration::from_millis(300))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_process_stopped(read_pid(&pid_file));
    }

    #[test]
    fn successful_parent_does_not_leave_background_children() {
        let output = Command::new("sh")
            .args(["-c", "sleep 30 & echo $!"])
            .output_before(Instant::now() + Duration::from_secs(3))
            .unwrap();
        assert!(output.status.success());
        assert_process_stopped(
            String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
    }

    #[test]
    fn expired_budget_does_not_spawn_command() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("spawned");
        let error = Command::new("touch")
            .arg(&marker)
            .output_before(Instant::now())
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(!marker.exists());
    }

    #[test]
    fn dropping_child_cleans_up_without_explicit_wait() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let child = Command::new("sh")
            .args(["-c", "sleep 30 & echo $! > \"$1\"; wait", "test"])
            .arg(&pid_file)
            .spawn_managed()
            .unwrap();
        let descendant = read_pid(&pid_file);
        let parent = child.id() as i32;
        drop(child);
        assert_process_stopped(parent);
        assert_process_stopped(descendant);
    }

    #[test]
    fn trickling_bytes_do_not_reset_read_deadline() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..20 {
                if stream.write_all(&[1]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(30));
            }
        });
        let started = Instant::now();
        let mut stream = DeadlineStream::new(
            TcpStream::connect(address).unwrap(),
            started + Duration::from_millis(150),
        );
        let error = stream.read_exact(&mut [0; 100]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(stream);
        server.join().unwrap();
    }

    // Run in a subprocess so signal handlers cannot affect parallel tests.
    #[test]
    fn cancellation_helper() {
        let Some(path) = std::env::var_os("FLATPAK_SMOKE_CANCELLATION_TEST") else {
            return;
        };
        install_signal_handlers().unwrap();
        let error = Command::new("sh")
            .args(["-c", "sleep 30 & echo $! > \"$1\"; wait", "test"])
            .arg(path)
            .output_before(Instant::now() + Duration::from_secs(20))
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn cancellation_cleans_up_descendants() {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let temp = tempfile::tempdir().unwrap();
            let pid_file = temp.path().join("child.pid");
            let mut helper = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process::tests::cancellation_helper",
                    "--nocapture",
                ])
                .env("FLATPAK_SMOKE_CANCELLATION_TEST", &pid_file)
                .spawn_managed()
                .unwrap();
            let descendant = read_pid(&pid_file);
            unsafe {
                libc::kill(helper.id() as i32, signal);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            let status = loop {
                if let Some(status) = helper.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "cancellation did not finish");
                thread::sleep(Duration::from_millis(10));
            };
            assert!(status.success());
            assert_process_stopped(descendant);
        }
    }
}
