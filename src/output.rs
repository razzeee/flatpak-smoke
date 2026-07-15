use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use crate::result::RunResult;

#[derive(Debug, Clone)]
pub struct OutputLayout {
    pub screenshots_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub result_json: PathBuf,
    pub app_stdout: PathBuf,
    pub app_stderr: PathBuf,
    pub runner_log: PathBuf,
}

impl OutputLayout {
    pub fn prepare(root: &Path, force: bool) -> anyhow::Result<Self> {
        if root.exists() {
            let protected = [
                root.join("result.json"),
                root.join("screenshots"),
                root.join("logs"),
            ];
            let existing: Vec<_> = protected.iter().filter(|path| path.exists()).collect();
            if !existing.is_empty() && !force {
                bail!(
                    "output directory '{}' already contains flatpak-smoke artifacts; pass --force to overwrite",
                    root.display()
                );
            }

            if force {
                remove_if_exists(&root.join("result.json"))?;
                remove_if_exists(&root.join("screenshots"))?;
                remove_if_exists(&root.join("logs"))?;
            }
        }

        fs::create_dir_all(root)
            .with_context(|| format!("creating output directory '{}'", root.display()))?;
        let screenshots_dir = root.join("screenshots");
        let logs_dir = root.join("logs");
        fs::create_dir_all(&screenshots_dir).with_context(|| {
            format!(
                "creating screenshots directory '{}'",
                screenshots_dir.display()
            )
        })?;
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("creating logs directory '{}'", logs_dir.display()))?;

        Ok(Self {
            screenshots_dir,
            logs_dir: logs_dir.clone(),
            result_json: root.join("result.json"),
            app_stdout: logs_dir.join("app.stdout.log"),
            app_stderr: logs_dir.join("app.stderr.log"),
            runner_log: logs_dir.join("runner.log"),
        })
    }

    pub fn screenshot_path(&self, screenshot_name: &str) -> PathBuf {
        self.screenshots_dir.join(screenshot_name)
    }

    pub fn relative_screenshot_path(&self, screenshot_name: &str) -> String {
        format!("screenshots/{screenshot_name}")
    }

    pub fn write_result(&self, result: &RunResult) -> anyhow::Result<()> {
        let file = File::create(&self.result_json)
            .with_context(|| format!("creating result file '{}'", self.result_json.display()))?;
        serde_json::to_writer_pretty(file, result).context("writing result.json")?;
        Ok(())
    }

    pub fn append_runner_log(&self, message: impl AsRef<str>) -> anyhow::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.runner_log)
            .with_context(|| format!("opening runner log '{}'", self.runner_log.display()))?;
        writeln!(file, "{}", message.as_ref()).context("writing runner log")?;
        Ok(())
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_existing_artifacts_without_force() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("screenshots")).unwrap();

        let error = OutputLayout::prepare(temp.path(), false).unwrap_err();
        assert!(error.to_string().contains("--force"));
    }

    #[test]
    fn force_removes_previous_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("result.json"), "old").unwrap();
        fs::create_dir_all(temp.path().join("logs")).unwrap();
        fs::write(temp.path().join("logs/old.log"), "old").unwrap();

        let layout = OutputLayout::prepare(temp.path(), true).unwrap();

        assert!(layout.result_json.ends_with("result.json"));
        assert!(!temp.path().join("logs/old.log").exists());
    }
}
