use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResult {
    pub schema_version: u8,
    pub status: RunStatus,
    pub app_ref: Option<String>,
    pub phase_metadata: PhaseMetadata,
    pub artifact: Artifact,
    pub timings_ms: Timings,
    pub screenshots: Vec<String>,
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateMetadata {
    pub from: Option<Artifact>,
    pub before: PhaseMetadata,
    pub after: PhaseMetadata,
    pub failed_phase: Option<String>,
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
            phase_metadata: PhaseMetadata::default(),
            artifact,
            timings_ms,
            screenshots,
            failure: None,
            update: None,
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
            phase_metadata: PhaseMetadata::default(),
            artifact,
            timings_ms,
            screenshots,
            failure: Some(Failure {
                reason,
                category: None,
                message: message.into(),
            }),
            update: None,
        }
    }

    pub fn is_passed(&self) -> bool {
        self.status == RunStatus::Passed
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseMetadata {
    pub app: Option<InstalledRef>,
    pub runtimes: Option<Vec<InstalledRef>>,
    pub recipe: Option<RecipeMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledRef {
    #[serde(rename = "ref")]
    pub reference: String,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecipeMetadata {
    pub content_hash: String,
    #[serde(default)]
    pub input_digest: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FailureCategory>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Infrastructure,
    Expectation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    InstallFailed,
    DependencyFailed,
    DisplayStartFailed,
    DisplayExited,
    LaunchFailed,
    WindowTimeout,
    EarlyExit,
    ShutdownFailed,
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
    fn failure_category_is_additive_and_optional() {
        let old = serde_json::json!({"reason": "screenshot_failed", "message": "failed"});
        let mut failure: Failure = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(failure.category, None);
        assert_eq!(serde_json::to_value(&failure).unwrap(), old);
        for (category, name) in [
            (FailureCategory::Infrastructure, "infrastructure"),
            (FailureCategory::Expectation, "expectation"),
        ] {
            failure.category = Some(category);
            let value = serde_json::to_value(&failure).unwrap();
            assert_eq!(value["category"], name);
            assert_eq!(value["reason"], "screenshot_failed");
            assert_eq!(serde_json::from_value::<Failure>(value).unwrap(), failure);
        }
    }

    #[test]
    fn recipe_metadata_accepts_legacy_results_and_round_trips_input_digest() {
        let mut metadata: RecipeMetadata =
            serde_json::from_str(r#"{"content_hash":"md5:legacy"}"#).unwrap();
        assert_eq!(metadata.input_digest, None);
        metadata.input_digest = Some(format!("v1:sha256:{}", "0".repeat(64)));
        let value = serde_json::to_value(&metadata).unwrap();
        assert_eq!(value["content_hash"], "md5:legacy");
        assert_eq!(
            serde_json::from_value::<RecipeMetadata>(value).unwrap(),
            metadata
        );
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
        assert!(value["phase_metadata"]["app"].is_null());
        assert!(value["phase_metadata"]["runtimes"].is_null());
        assert!(value["phase_metadata"]["recipe"].is_null());
    }

    #[test]
    fn serializes_phase_metadata_and_preserves_missing_commit() {
        let metadata = PhaseMetadata {
            app: Some(InstalledRef {
                reference: "app/org.example.App/x86_64/stable".into(),
                commit: None,
            }),
            runtimes: Some(vec![InstalledRef {
                reference: "runtime/org.freedesktop.Platform/x86_64/24.08".into(),
                commit: Some("abc123".into()),
            }]),
            recipe: Some(RecipeMetadata {
                content_hash: "md5:d41d8cd98f00b204e9800998ecf8427e".into(),
                input_digest: None,
            }),
        };
        let value = serde_json::to_value(metadata).unwrap();
        assert!(value["app"]["commit"].is_null());
        assert_eq!(value["app"]["ref"], "app/org.example.App/x86_64/stable");
        assert_eq!(value["runtimes"][0]["commit"], "abc123");
        assert_eq!(
            value["recipe"]["content_hash"],
            "md5:d41d8cd98f00b204e9800998ecf8427e"
        );
    }
}
