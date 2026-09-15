use anyhow::{Context, bail, ensure};
use serde::Deserialize;
use std::{collections::HashSet, fs, path::Path, time::Duration};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub version: u32,
    #[serde(default = "gnome")]
    pub desktop: String,
    #[serde(default = "english")]
    pub language: String,
    #[serde(default)]
    pub window: Window,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            title: None,
            width: 900,
            height: 600,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub name: String,
    pub caption: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub title: String,
}

#[derive(Debug)]
pub enum Action {
    Capture(Capture),
    ClickText(String),
    TypeText(String),
    Key(Vec<u32>),
    WaitText(String),
    SelectWindow(Selection),
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "RawStep")]
pub struct Step {
    pub timeout: Option<Duration>,
    pub action: Action,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    timeout: Option<String>,
    capture: Option<Capture>,
    click_text: Option<String>,
    type_text: Option<String>,
    key: Option<String>,
    wait_text: Option<String>,
    select_window: Option<Selection>,
}

impl TryFrom<RawStep> for Step {
    type Error = anyhow::Error;
    fn try_from(raw: RawStep) -> anyhow::Result<Self> {
        let actions: Vec<_> = [
            raw.capture.map(Action::Capture),
            raw.click_text.map(Action::ClickText),
            raw.type_text.map(Action::TypeText),
            raw.wait_text.map(Action::WaitText),
            raw.select_window.map(Action::SelectWindow),
            raw.key
                .as_deref()
                .map(key_values)
                .transpose()?
                .map(Action::Key),
        ]
        .into_iter()
        .flatten()
        .collect();
        ensure!(
            actions.len() == 1,
            "each step must contain exactly one action"
        );
        let action = actions.into_iter().next().expect("validated action count");
        match &action {
            Action::Capture(capture) => {
                ensure!(
                    !capture.name.is_empty()
                        && capture.name.len() <= 100
                        && capture
                            .name
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
                    "capture name must contain only letters, digits, '-' or '_'"
                );
                nonempty(&capture.caption, "caption")?;
                ensure!(
                    !capture.caption.contains(['\n', '\r'])
                        && !capture.caption.trim_end().ends_with('.'),
                    "caption must be a single line without a trailing full stop"
                );
            }
            Action::ClickText(text) => nonempty(text, "click text")?,
            Action::WaitText(text) => nonempty(text, "wait text")?,
            Action::TypeText(text) => ensure!(
                !text.is_empty() && !text.contains('\0'),
                "type_text must contain nonempty text without NUL"
            ),
            Action::SelectWindow(selection) => nonempty(&selection.title, "window title")?,
            Action::Key(_) => {}
        }
        let timeout = raw
            .timeout
            .as_deref()
            .map(crate::cli::parse_duration)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        Ok(Self { timeout, action })
    }
}

fn gnome() -> String {
    "gnome".into()
}
fn english() -> String {
    "en".into()
}

impl Recipe {
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path).context("reading screenshot recipe")?;
        let recipe: Self = serde_yaml_ng::from_str(&text).context("parsing screenshot recipe")?;
        ensure!(
            recipe.version == 1,
            "unsupported recipe version {}",
            recipe.version
        );
        ensure!(
            recipe.desktop == "gnome",
            "unsupported desktop '{}'",
            recipe.desktop
        );
        ensure!(
            recipe.language == "en",
            "unsupported screenshot language '{}'",
            recipe.language
        );
        ensure!(
            (1..=1000).contains(&recipe.window.width) && (1..=700).contains(&recipe.window.height),
            "window size must be within 1000x700 logical pixels"
        );
        if let Some(title) = &recipe.window.title {
            nonempty(title, "window title")?;
        }
        let mut names = HashSet::new();
        for step in &recipe.steps {
            if let Action::Capture(capture) = &step.action {
                ensure!(
                    names.insert(&capture.name),
                    "duplicate capture name '{}'",
                    capture.name
                );
            }
        }
        ensure!(
            !names.is_empty(),
            "recipe must contain at least one capture"
        );
        Ok(recipe)
    }
}

fn nonempty(value: &str, what: &str) -> anyhow::Result<()> {
    ensure!(
        !value.trim().is_empty() && !value.contains('\0'),
        "{what} must not be empty or contain NUL"
    );
    Ok(())
}

fn key_values(chord: &str) -> anyhow::Result<Vec<u32>> {
    let parts: Vec<_> = chord.split('+').collect();
    let mut values = Vec::new();
    for modifier in &parts[..parts.len() - 1] {
        let value = match *modifier {
            "Ctrl" => 0xffe3,
            "Shift" => 0xffe1,
            "Alt" => 0xffe9,
            "Super" => 0xffeb,
            _ => bail!("unsupported key modifier '{modifier}'"),
        };
        ensure!(
            !values.contains(&value),
            "duplicate key modifier '{modifier}'"
        );
        values.push(value);
    }
    let key = parts[parts.len() - 1];
    let value = match key {
        "Return" => 0xff0d,
        "Escape" => 0xff1b,
        "Tab" => 0xff09,
        "BackSpace" => 0xff08,
        "Delete" => 0xffff,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "F1" => 0xffbe,
        "F2" => 0xffbf,
        "F3" => 0xffc0,
        "F4" => 0xffc1,
        "F5" => 0xffc2,
        "F6" => 0xffc3,
        "F7" => 0xffc4,
        "F8" => 0xffc5,
        "F9" => 0xffc6,
        "F10" => 0xffc7,
        "F11" => 0xffc8,
        "F12" => 0xffc9,
        "space" => 0x20,
        "comma" => 0x2c,
        "plus" => 0x2b,
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_graphic() => u32::from(key.as_bytes()[0]),
        _ => bail!("unsupported key '{key}'"),
    };
    values.push(value);
    Ok(values)
}
