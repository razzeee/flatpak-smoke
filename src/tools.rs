use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::bail;

pub const REQUIRED_TOOLS: &[&str] = &[
    "flatpak",
    "dbus-run-session",
    "gnome-keyring-daemon",
    "weston",
    "weston-screenshooter",
    "tesseract",
    "compare",
];

const REQUIRED_PATHS: &[RequiredPath] = &[
    RequiredPath::executable("xdg-desktop-portal", "/usr/libexec/xdg-desktop-portal"),
    RequiredPath::executable(
        "xdg-desktop-portal-gtk",
        "/usr/libexec/xdg-desktop-portal-gtk",
    ),
    RequiredPath::file(
        "gnome-keyring Secret portal backend",
        "/usr/share/xdg-desktop-portal/portals/gnome-keyring.portal",
    ),
];

pub fn doctor() -> anyhow::Result<()> {
    let missing = missing_requirements();
    if missing.is_empty() {
        println!("flatpak-smoke doctor: ok");
        return Ok(());
    }

    for tool in &missing {
        eprintln!("missing required tool: {tool}");
    }
    bail!("missing {} required tool(s)", missing.len())
}

pub fn ensure_required_tools() -> anyhow::Result<()> {
    let missing = missing_requirements();
    if missing.is_empty() {
        Ok(())
    } else {
        bail!("missing required tool(s): {}", missing.join(", "))
    }
}

fn missing_requirements() -> Vec<String> {
    let mut missing = missing_tools(REQUIRED_TOOLS);
    missing.extend(missing_paths(REQUIRED_PATHS));
    missing
}

pub fn missing_tools(tools: &[&str]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| find_on_path(tool).is_none())
        .map(|tool| (*tool).to_string())
        .collect()
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 && is_executable(candidate) {
        return Some(candidate.to_path_buf());
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(OsStr::new(program));
        is_executable(&candidate).then_some(candidate)
    })
}

fn missing_paths(paths: &[RequiredPath]) -> Vec<String> {
    paths
        .iter()
        .filter(|required| !required.is_present())
        .map(|required| required.label.to_string())
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct RequiredPath {
    label: &'static str,
    path: &'static str,
    executable: bool,
}

impl RequiredPath {
    const fn executable(label: &'static str, path: &'static str) -> Self {
        Self {
            label,
            path,
            executable: true,
        }
    }

    const fn file(label: &'static str, path: &'static str) -> Self {
        Self {
            label,
            path,
            executable: false,
        }
    }

    fn is_present(&self) -> bool {
        let path = Path::new(self.path);
        if self.executable {
            is_executable(path)
        } else {
            path.is_file()
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

pub fn check_path(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.exists() {
        Ok(())
    } else {
        bail!("{label} '{}' does not exist", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_unknown_tool() {
        let missing = missing_tools(&["definitely-not-a-flatpak-smoke-tool"]);
        assert_eq!(missing, vec!["definitely-not-a-flatpak-smoke-tool"]);
    }

    #[test]
    fn reports_missing_required_paths_by_label() {
        let missing = missing_paths(&[RequiredPath::file(
            "definitely missing file",
            "/definitely/not/a/flatpak-smoke-file",
        )]);

        assert_eq!(missing, vec!["definitely missing file"]);
    }
}
