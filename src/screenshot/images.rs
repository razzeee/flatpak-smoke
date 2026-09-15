use super::{desktop::Window, workspace::Workspace};
use crate::{
    process::{BoundedCommand, HealthCheck},
    session::find_ocr_text_matches,
};
use anyhow::{Context, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub struct Images<'a> {
    pub workspace: &'a Workspace,
    pub logs: &'a Path,
    pub health: HealthCheck,
}

pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub has_content: bool,
}

impl Images<'_> {
    pub fn inspect(
        &self,
        path: &Path,
        window: &Window,
        deadline: Instant,
    ) -> anyhow::Result<ImageInfo> {
        let output = self
            .workspace
            .command("identify")
            .args(["-format", "%m %w %h"])
            .arg(path)
            .output_before_checked(deadline, &self.health)?;
        ensure!(
            output.status.success(),
            "invalid capture image: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout)?;
        let fields: Vec<_> = text.split_whitespace().collect();
        ensure!(
            fields.len() == 3 && fields[0] == "PNG",
            "capture must be one PNG image"
        );
        let width: u32 = fields[1].parse()?;
        let height: u32 = fields[2].parse()?;
        ensure!(width > 0 && height > 0, "empty capture image");
        // Measure the frame interior, not the alpha/shadow border. Reserve the
        // top strip for ordinary native/CSD headers so a titlebar above a blank
        // client does not count as application content. Listing pixels remain
        // untouched; this crop is used only for the content measurement.
        let header = (window.frame.height / 4).min(64);
        let inset = 8;
        let x = i64::from(window.frame.x) - i64::from(window.buffer.x) + inset;
        let y = i64::from(window.frame.y) - i64::from(window.buffer.y) + i64::from(header) + inset;
        let content_width = window.frame.width.saturating_sub(16);
        let content_height = window.frame.height.saturating_sub(header + 16);
        ensure!(
            x >= 0
                && y >= 0
                && content_width > 0
                && content_height > 0
                && x + i64::from(content_width) <= i64::from(width)
                && y + i64::from(content_height) <= i64::from(height),
            "invalid window content geometry"
        );
        let geometry = format!("{content_width}x{content_height}+{x}+{y}");
        let output = self
            .workspace
            .command("convert")
            .arg(path)
            .args([
                "-crop",
                &geometry,
                "+repage",
                "-background",
                "white",
                "-alpha",
                "remove",
                "-alpha",
                "off",
                "-format",
                "%[fx:standard_deviation]",
                "info:",
            ])
            .output_before_checked(deadline, &self.health)?;
        ensure!(
            output.status.success(),
            "measuring app content: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deviation: f64 = String::from_utf8(output.stdout)?.trim().parse()?;
        Ok(ImageInfo {
            width,
            height,
            has_content: deviation.is_finite() && deviation > 0.01,
        })
    }

    pub fn ocr(&self, native: &Path, deadline: Instant) -> anyhow::Result<Vec<String>> {
        let diagnostic = self.logs.join("ocr.png");
        let output = self
            .workspace
            .command("convert")
            .arg(native)
            .args([
                "-background",
                "white",
                "-alpha",
                "remove",
                "-alpha",
                "off",
                "-resize",
                "200%",
            ])
            .arg(&diagnostic)
            .output_before_checked(deadline, &self.health)?;
        ensure!(
            output.status.success(),
            "preparing OCR image: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let contrast = self.logs.join("ocr-contrast.png");
        let output = self
            .workspace
            .command("convert")
            .arg(&diagnostic)
            .args(["-colorspace", "Gray", "-threshold", "60%"])
            .arg(&contrast)
            .output_before_checked(deadline, &self.health)?;
        ensure!(
            output.status.success(),
            "preparing contrast OCR image: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut passes = Vec::new();
        for (image, name) in [(diagnostic, "ocr.tsv"), (contrast, "ocr-contrast.tsv")] {
            let output = self
                .workspace
                .command("tesseract")
                .arg(image)
                .args(["stdout", "-l", "eng", "--psm", "11", "tsv"])
                .output_before_checked(deadline, &self.health)?;
            ensure!(
                output.status.success(),
                "reading screenshot text: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let tsv = String::from_utf8(output.stdout).context("invalid OCR text encoding")?;
            fs::write(self.logs.join(name), &tsv)?;
            passes.push(tsv);
        }
        Ok(passes)
    }

    pub fn matches(&self, passes: &[String], label: &str) -> Vec<(f64, f64)> {
        let mut matches: Vec<(f64, f64)> = Vec::new();
        for pass in passes {
            let previous_passes = matches.clone();
            for (x, y) in find_ocr_text_matches(pass, label) {
                let point = (f64::from(x) / 2.0, f64::from(y) / 2.0);
                // The two preprocessing passes may recognize the same label with
                // slightly different bounds. Keep separate physical locations.
                if !previous_passes
                    .iter()
                    .any(|other| (other.0 - point.0).hypot(other.1 - point.1) <= 4.0)
                {
                    matches.push(point);
                }
            }
        }
        matches
    }

    pub fn candidate_path(&self) -> PathBuf {
        self.logs.join("last-candidate.png")
    }
}
