use crate::process::remaining;
use anyhow::ensure;
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Instant,
};

pub fn expand(argument: &str, profile: &Path) -> anyhow::Result<String> {
    ensure!(
        !argument.contains('\0'),
        "launch arguments must not contain NUL"
    );
    let mut result = String::new();
    let mut rest = argument;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let placeholder = &rest[start + 2..];
        let end = placeholder
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("unterminated launch placeholder"))?;
        let directory = match &placeholder[..end] {
            "APP_DATA" => "data",
            "APP_CONFIG" => "config",
            other => anyhow::bail!(
                "unsupported launch placeholder '${{{other}}}'; use APP_DATA or APP_CONFIG"
            ),
        };
        result.push_str(
            profile
                .join(directory)
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("app profile path is not UTF-8"))?,
        );
        rest = &placeholder[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

pub fn copy(source: &Path, destination: &Path, deadline: Instant) -> anyhow::Result<()> {
    remaining(deadline)?;
    for ancestor in source
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
    {
        ensure!(
            !fs::symlink_metadata(ancestor)?.file_type().is_symlink(),
            "setup source contains a symlink: {}",
            ancestor.display()
        );
    }
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy(
                &entry.path(),
                &destination.join(entry.file_name()),
                deadline,
            )?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "source must be a regular file or directory"
        );
        fs::create_dir_all(
            destination
                .parent()
                .expect("profile destination has a parent"),
        )?;
        let mut input = fs::File::open(source)?;
        let mut output = fs::File::create_new(destination)?;
        let mut buffer = [0; 65536];
        loop {
            remaining(deadline)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
        }
    }
    Ok(())
}
