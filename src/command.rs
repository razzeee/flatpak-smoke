use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct CommandRunner {
    runner_log: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl CommandRunner {
    pub fn new(runner_log: impl Into<PathBuf>) -> Self {
        Self {
            runner_log: runner_log.into(),
            env: Vec::new(),
        }
    }

    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn run<I, S>(
        &self,
        program: &str,
        args: I,
        timeout: Duration,
    ) -> anyhow::Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_os_string())
            .collect();
        self.log_command(program, &args)?;

        let mut stdout_file = tempfile::tempfile().context("creating command stdout temp file")?;
        let mut stderr_file = tempfile::tempfile().context("creating command stderr temp file")?;
        let stdout_for_child = stdout_file
            .try_clone()
            .context("cloning command stdout temp file")?;
        let stderr_for_child = stderr_file
            .try_clone()
            .context("cloning command stderr temp file")?;

        let mut child = self
            .command(program)
            .args(&args)
            .stdout(Stdio::from(stdout_for_child))
            .stderr(Stdio::from(stderr_for_child))
            .spawn()
            .with_context(|| format!("spawning {program}"))?;

        let started = Instant::now();
        loop {
            if child.try_wait()?.is_some() {
                let status = child.wait()?;
                let result = CommandOutput {
                    status,
                    stdout: read_temp_file(&mut stdout_file).context("reading command stdout")?,
                    stderr: read_temp_file(&mut stderr_file).context("reading command stderr")?,
                };
                self.log_status(program, &result)?;
                return Ok(result);
            }

            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                let stdout = read_temp_file(&mut stdout_file).unwrap_or_default();
                let stderr = read_temp_file(&mut stderr_file).unwrap_or_default();
                self.append_log(format!(
                    "{program} timed out after {}",
                    format_duration(timeout)
                ))?;
                if !stdout.trim().is_empty() {
                    self.append_log(format!(
                        "{program} stdout before timeout:\n{}",
                        stdout.trim_end()
                    ))?;
                }
                if !stderr.trim().is_empty() {
                    self.append_log(format!(
                        "{program} stderr before timeout:\n{}",
                        stderr.trim_end()
                    ))?;
                }
                bail!(
                    "command '{program}' timed out after {}",
                    format_duration(timeout)
                );
            }

            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    fn log_command(&self, program: &str, args: &[OsString]) -> anyhow::Result<()> {
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        self.append_log(format!("$ {program} {args}"))
    }

    fn log_status(&self, program: &str, output: &CommandOutput) -> anyhow::Result<()> {
        self.append_log(format!("{program} exited with {}", output.status))?;
        if !output.stdout.trim().is_empty() {
            self.append_log(format!("{program} stdout:\n{}", output.stdout.trim_end()))?;
        }
        if !output.stderr.trim().is_empty() {
            self.append_log(format!("{program} stderr:\n{}", output.stderr.trim_end()))?;
        }
        Ok(())
    }

    fn append_log(&self, message: impl AsRef<str>) -> anyhow::Result<()> {
        if let Some(parent) = self.runner_log.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating log directory '{}'", parent.display()))?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.runner_log)
            .with_context(|| format!("opening runner log '{}'", self.runner_log.display()))?;
        writeln!(file, "{}", message.as_ref()).context("writing runner log")?;
        Ok(())
    }
}

fn read_temp_file(file: &mut File) -> anyhow::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis.is_multiple_of(1_000) {
        format!("{}s", millis / 1_000)
    } else {
        format!("{}.{:03}s", millis / 1_000, millis % 1_000)
    }
}

pub fn ensure_file_nonempty(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("reading '{}'", path.display()))?;
    if metadata.len() == 0 {
        bail!("'{}' is empty", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_subsecond_timeouts() {
        assert_eq!(format_duration(Duration::from_millis(250)), "250ms");
        assert_eq!(format_duration(Duration::from_secs(2)), "2s");
        assert_eq!(format_duration(Duration::from_millis(1250)), "1.250s");
    }
}
