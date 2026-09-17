use crate::process::HealthCheck;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Window {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub transient: bool,
    pub frame: Rect,
    pub buffer: Rect,
    pub scale: f64,
}

pub enum Input<'a> {
    Click(f64, f64),
    Key(&'a [u32]),
    Text(&'a str),
}

/// Operations required by recipes, independent of the desktop's transport/API.
pub trait Desktop {
    fn version(&self) -> &str;
    fn health(&self) -> HealthCheck;
    fn check(&self, deadline: Instant) -> anyhow::Result<()>;
    fn windows(&self, deadline: Instant) -> anyhow::Result<Vec<Window>>;
    fn resize(
        &self,
        window: u64,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> anyhow::Result<Window>;
    fn capture(&self, window: u64, path: &Path, deadline: Instant) -> anyhow::Result<Window>;
    fn input(&self, window: u64, input: Input<'_>, deadline: Instant) -> anyhow::Result<()>;
}
