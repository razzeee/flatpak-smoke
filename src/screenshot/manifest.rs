use serde::Serialize;
use std::{fs::File, path::Path};

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub schema_version: u8,
    pub desktop: String,
    pub desktop_version: Option<String>,
    pub language: String,
    pub scale: u32,
    pub recipe_version: u32,
    pub app_ref: Option<String>,
    pub failed_step_index: Option<usize>,
    pub captures: Vec<Capture>,
}

#[derive(Debug, Serialize)]
pub struct Capture {
    pub name: String,
    pub path: String,
    pub caption: String,
    pub language: String,
    pub width: u32,
    pub height: u32,
    pub window_width: u32,
    pub window_height: u32,
}

impl Manifest {
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            desktop: "gnome".into(),
            desktop_version: None,
            language: "en".into(),
            scale: 1,
            recipe_version: 1,
            app_ref: None,
            failed_step_index: None,
            captures: Vec::new(),
        }
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        serde_json::to_writer_pretty(File::create(path)?, self)?;
        Ok(())
    }
}
