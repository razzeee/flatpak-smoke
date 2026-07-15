use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResult {
    pub schema_version: u8,
    pub status: RunStatus,
    pub app_ref: Option<String>,
    pub artifact: Artifact,
    pub timings_ms: Timings,
    pub screenshots: Vec<String>,
    pub failure: Option<Failure>,
}

impl RunResult {
    pub fn passed(
        app_ref: String,
        artifact: Artifact,
        timings_ms: Timings,
        screenshots: Vec<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            status: RunStatus::Passed,
            app_ref: Some(app_ref),
            artifact,
            timings_ms,
            screenshots,
            failure: None,
        }
    }

    pub fn failed(
        app_ref: Option<String>,
        artifact: Artifact,
        timings_ms: Timings,
        screenshots: Vec<String>,
        reason: FailureReason,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: 1,
            status: RunStatus::Failed,
            app_ref,
            artifact,
            timings_ms,
            screenshots,
            failure: Some(Failure {
                reason,
                message: message.into(),
            }),
        }
    }

    pub fn is_passed(&self) -> bool {
        self.status == RunStatus::Passed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub path: String,
}

impl Artifact {
    pub fn bundle(path: &Path) -> Self {
        Self {
            kind: ArtifactKind::Bundle,
            path: path.display().to_string(),
        }
    }

    pub fn repo(path: &Path) -> Self {
        Self {
            kind: ArtifactKind::Repo,
            path: path.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Bundle,
    Repo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Timings {
    pub install: Option<u128>,
    pub launch_to_window: Option<u128>,
    pub total: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    pub reason: FailureReason,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    InstallFailed,
    DependencyFailed,
    DisplayStartFailed,
    LaunchFailed,
    WindowTimeout,
    EarlyExit,
    ScreenshotFailed,
    AppErrorWindow,
    InternalError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_failure_reason_as_stable_snake_case() {
        let json = serde_json::to_string(&FailureReason::WindowTimeout).unwrap();
        assert_eq!(json, "\"window_timeout\"");

        let json = serde_json::to_string(&FailureReason::AppErrorWindow).unwrap();
        assert_eq!(json, "\"app_error_window\"");
    }

    #[test]
    fn serializes_result_shape() {
        let result = RunResult::passed(
            "app/org.example.App/x86_64/stable".to_string(),
            Artifact::bundle(Path::new("app.flatpak")),
            Timings {
                install: Some(1200),
                launch_to_window: Some(2400),
                total: 4100,
            },
            vec!["screenshots/000-window-visible.png".to_string()],
        );

        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["status"], "passed");
        assert_eq!(value["artifact"]["kind"], "bundle");
        assert_eq!(value["failure"], serde_json::Value::Null);
    }
}
