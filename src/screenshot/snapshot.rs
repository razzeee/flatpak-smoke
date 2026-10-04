use super::{
    desktop::{Desktop, Window},
    images::{ImageInfo, Images},
};
use anyhow::{Context, ensure};
use std::{fs, path::Path, time::Instant};
use tempfile::TempPath;

pub(super) struct CaptureSnapshot {
    window: Window,
    image: ImageInfo,
    scratch: TempPath,
}

impl CaptureSnapshot {
    pub(super) fn window(&self) -> &Window {
        &self.window
    }

    pub(super) fn image(&self) -> &ImageInfo {
        &self.image
    }

    pub(super) fn path(&self) -> &Path {
        self.scratch.as_ref()
    }

    pub(super) fn close(self) -> anyhow::Result<()> {
        self.scratch
            .close()
            .context("removing screenshot snapshot scratch image")
    }
}

pub(super) fn capture(
    desktop: &dyn Desktop,
    images: &Images<'_>,
    logs: &Path,
    window_id: u64,
    deadline: Instant,
) -> anyhow::Result<CaptureSnapshot> {
    capture_with_inspector(
        desktop,
        logs,
        window_id,
        deadline,
        |path, window, deadline| images.inspect(path, window, deadline),
    )
}

pub(super) fn capture_with_inspector<F>(
    desktop: &dyn Desktop,
    logs: &Path,
    window_id: u64,
    deadline: Instant,
    inspect: F,
) -> anyhow::Result<CaptureSnapshot>
where
    F: FnOnce(&Path, &Window, Instant) -> anyhow::Result<ImageInfo>,
{
    desktop.check(deadline)?;
    let scratch = tempfile::Builder::new()
        .prefix("capture-")
        .suffix(".png")
        .tempfile_in(logs)
        .context("creating screenshot snapshot scratch image")?
        .into_temp_path();
    let result = (|| {
        let window = desktop.capture(window_id, &scratch, deadline)?;
        let image = inspect(&scratch, &window, deadline)?;
        ensure!(
            window.scale == 1.0,
            "unsupported window scale {}; expected 1",
            window.scale
        );
        ensure!(
            window.frame.width <= 1000 && window.frame.height <= 700,
            "window size {}x{} exceeds 1000x700",
            window.frame.width,
            window.frame.height
        );
        ensure!(
            image.width == window.buffer.width && image.height == window.buffer.height,
            "native image and window buffer dimensions differ"
        );

        promote_candidate(&scratch, logs)?;
        Ok((window, image))
    })();
    let ((window, image), scratch) = finish_scratch(result, scratch)?;
    Ok(CaptureSnapshot {
        window,
        image,
        scratch,
    })
}

fn promote_candidate(scratch: &Path, logs: &Path) -> anyhow::Result<()> {
    let candidate = logs.join("last-candidate.png");
    let staged = tempfile::Builder::new()
        .prefix("candidate-")
        .suffix(".png")
        .tempfile_in(logs)
        .context("staging screenshot candidate")?
        .into_temp_path();
    fs::copy(scratch, &staged).context("copying validated screenshot candidate")?;
    staged
        .persist(&candidate)
        .context("promoting validated screenshot candidate")?;
    Ok(())
}

fn finish_scratch<T>(
    result: anyhow::Result<T>,
    scratch: TempPath,
) -> anyhow::Result<(T, TempPath)> {
    match result {
        Ok(value) => Ok((value, scratch)),
        Err(error) => match scratch.close() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!(
                "also failed to remove screenshot snapshot scratch image: {cleanup}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{capture_with_inspector, finish_scratch, promote_candidate};
    use crate::{
        process::HealthCheck,
        screenshot::{
            desktop::{Desktop, Input, Rect, Window},
            images::ImageInfo,
        },
    };
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    struct FakeDesktop {
        window: Window,
        capture_error: bool,
    }

    impl Desktop for FakeDesktop {
        fn version(&self) -> &str {
            "test"
        }

        fn health(&self) -> HealthCheck {
            HealthCheck::default()
        }

        fn check(&self, _deadline: Instant) -> anyhow::Result<()> {
            Ok(())
        }

        fn windows(&self, _deadline: Instant) -> anyhow::Result<Vec<Window>> {
            Ok(vec![self.window.clone()])
        }

        fn resize(
            &self,
            _window: u64,
            _width: u32,
            _height: u32,
            _deadline: Instant,
        ) -> anyhow::Result<Window> {
            Ok(self.window.clone())
        }

        fn capture(&self, _window: u64, path: &Path, _deadline: Instant) -> anyhow::Result<Window> {
            if self.capture_error {
                anyhow::bail!("capture failed");
            }
            fs::write(path, b"native png bytes")?;
            Ok(self.window.clone())
        }

        fn input(&self, _window: u64, _input: Input<'_>, _deadline: Instant) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn window() -> Window {
        Window {
            id: 7,
            title: "Main".into(),
            app_id: "org.example.Test".into(),
            transient: false,
            frame: Rect {
                x: 0,
                y: 0,
                width: 900,
                height: 600,
            },
            buffer: Rect {
                x: 0,
                y: 0,
                width: 900,
                height: 600,
            },
            scale: 1.0,
        }
    }

    fn image() -> ImageInfo {
        ImageInfo {
            width: 900,
            height: 600,
            has_content: true,
        }
    }

    fn run_capture(
        desktop: &FakeDesktop,
        logs: &Path,
        info: ImageInfo,
    ) -> anyhow::Result<super::CaptureSnapshot> {
        capture_with_inspector(
            desktop,
            logs,
            desktop.window.id,
            Instant::now() + Duration::from_secs(2),
            move |_, _, _| Ok(info),
        )
    }

    #[test]
    fn capture_returns_matching_window_and_image() {
        let output = tempfile::tempdir().unwrap();
        let desktop = FakeDesktop {
            window: window(),
            capture_error: false,
        };
        let snapshot = run_capture(&desktop, output.path(), image()).unwrap();

        assert_eq!(snapshot.window().id, desktop.window.id);
        assert_eq!(snapshot.image().width, 900);
        assert_eq!(snapshot.image().height, 600);
        assert_eq!(fs::read(snapshot.path()).unwrap(), b"native png bytes");
        assert_eq!(
            fs::read(output.path().join("last-candidate.png")).unwrap(),
            b"native png bytes"
        );
        snapshot.close().unwrap();
    }

    #[test]
    fn each_capture_owns_a_distinct_stable_path() {
        let output = tempfile::tempdir().unwrap();
        let desktop = FakeDesktop {
            window: window(),
            capture_error: false,
        };
        let first = run_capture(&desktop, output.path(), image()).unwrap();
        let first_path = first.path().to_path_buf();
        let second = run_capture(&desktop, output.path(), image()).unwrap();

        assert_ne!(first.path(), second.path());
        assert_eq!(fs::read(&first_path).unwrap(), b"native png bytes");
        assert_eq!(fs::read(second.path()).unwrap(), b"native png bytes");
        first.close().unwrap();
        second.close().unwrap();
    }

    #[test]
    fn invalid_scale_does_not_promote_candidate() {
        let output = tempfile::tempdir().unwrap();
        let mut invalid = window();
        invalid.scale = 2.0;
        let desktop = FakeDesktop {
            window: invalid,
            capture_error: false,
        };
        fs::write(
            output.path().join("last-candidate.png"),
            b"previous diagnostic",
        )
        .unwrap();

        let error = run_capture(&desktop, output.path(), image()).err().unwrap();

        assert!(format!("{error:#}").contains("unsupported window scale"));
        assert_eq!(
            fs::read(output.path().join("last-candidate.png")).unwrap(),
            b"previous diagnostic"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_frame_does_not_promote_candidate() {
        let output = tempfile::tempdir().unwrap();
        let mut invalid = window();
        invalid.frame.width = 1001;
        let desktop = FakeDesktop {
            window: invalid,
            capture_error: false,
        };
        let mut oversized_image = image();
        oversized_image.width = 1001;
        fs::write(
            output.path().join("last-candidate.png"),
            b"previous diagnostic",
        )
        .unwrap();

        let error = run_capture(&desktop, output.path(), oversized_image)
            .err()
            .unwrap();

        assert!(format!("{error:#}").contains("window size"));
        assert_eq!(
            fs::read(output.path().join("last-candidate.png")).unwrap(),
            b"previous diagnostic"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }

    #[test]
    fn dimension_mismatch_does_not_promote_candidate() {
        let output = tempfile::tempdir().unwrap();
        let desktop = FakeDesktop {
            window: window(),
            capture_error: false,
        };
        let mut mismatched = image();
        mismatched.width = 899;
        fs::write(
            output.path().join("last-candidate.png"),
            b"previous diagnostic",
        )
        .unwrap();

        let error = run_capture(&desktop, output.path(), mismatched)
            .err()
            .unwrap();

        assert!(format!("{error:#}").contains("dimensions differ"));
        assert_eq!(
            fs::read(output.path().join("last-candidate.png")).unwrap(),
            b"previous diagnostic"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }

    #[test]
    fn capture_error_cleans_scratch() {
        let output = tempfile::tempdir().unwrap();
        let desktop = FakeDesktop {
            window: window(),
            capture_error: true,
        };
        fs::write(
            output.path().join("last-candidate.png"),
            b"previous diagnostic",
        )
        .unwrap();
        let error = capture_with_inspector(
            &desktop,
            output.path(),
            desktop.window.id,
            Instant::now() + Duration::from_secs(2),
            |_, _, _| Ok(image()),
        )
        .err()
        .unwrap();

        assert!(format!("{error:#}").contains("capture failed"));
        assert_eq!(
            fs::read(output.path().join("last-candidate.png")).unwrap(),
            b"previous diagnostic"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
    }

    #[test]
    fn inspection_error_cleans_scratch() {
        let output = tempfile::tempdir().unwrap();
        let desktop = FakeDesktop {
            window: window(),
            capture_error: false,
        };
        let error = capture_with_inspector(
            &desktop,
            output.path(),
            desktop.window.id,
            Instant::now() + Duration::from_secs(2),
            |_, _, _| anyhow::bail!("inspection failed"),
        )
        .err()
        .unwrap();

        assert!(format!("{error:#}").contains("inspection failed"));
        assert!(!output.path().join("last-candidate.png").exists());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[test]
    fn capture_error_preserves_primary_error_when_scratch_cleanup_fails() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(root.path())
            .unwrap()
            .into_temp_path();
        let scratch_path = scratch.to_path_buf();
        fs::remove_file(&scratch_path).unwrap();
        fs::create_dir(&scratch_path).unwrap();
        fs::write(scratch_path.join("keep"), b"prevent directory removal").unwrap();

        let error =
            finish_scratch::<()>(Err(anyhow::anyhow!("capture failed")), scratch).unwrap_err();

        assert_eq!(error.root_cause().to_string(), "capture failed");
        assert!(format!("{error:#}").contains("also failed to remove"));
        assert!(scratch_path.join("keep").exists());
    }

    #[test]
    fn candidate_promotion_atomically_replaces_existing_diagnostic() {
        let logs = tempfile::tempdir().unwrap();
        let scratch = logs.path().join("scratch.png");
        let candidate = logs.path().join("last-candidate.png");
        fs::write(&scratch, b"new validated frame").unwrap();
        fs::write(&candidate, b"previous diagnostic").unwrap();

        promote_candidate(&scratch, logs.path()).unwrap();

        assert_eq!(fs::read(candidate).unwrap(), b"new validated frame");
        assert_eq!(fs::read(scratch).unwrap(), b"new validated frame");
        assert_eq!(fs::read_dir(logs.path()).unwrap().count(), 2);
    }

    #[test]
    fn failed_candidate_staging_preserves_existing_diagnostic() {
        let logs = tempfile::tempdir().unwrap();
        let candidate = logs.path().join("last-candidate.png");
        fs::write(&candidate, b"previous diagnostic").unwrap();
        let missing_scratch = logs.path().join("missing.png");

        let error = promote_candidate(&missing_scratch, logs.path()).unwrap_err();

        assert!(format!("{error:#}").contains("copying validated screenshot candidate"));
        assert_eq!(fs::read(candidate).unwrap(), b"previous diagnostic");
        assert_eq!(fs::read_dir(logs.path()).unwrap().count(), 1);
    }
}
