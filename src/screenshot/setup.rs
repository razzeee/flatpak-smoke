use crate::process::remaining;
use anyhow::ensure;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
    time::Instant,
};

/// Versioned, length-framed provenance of the bytes successfully seeded.
pub struct InputDigest(Sha256);

impl InputDigest {
    pub fn new(recipe_bytes: &[u8]) -> Self {
        let mut digest = Self(Sha256::new());
        digest.field(b"flatpak-smoke-recipe-input-v1");
        digest.field(recipe_bytes);
        digest
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    pub fn entry(&mut self) {
        self.field(b"setup");
    }

    pub fn finish(self) -> String {
        format!(
            "v1:sha256:{}",
            self.0
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }
}

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

pub fn copy(
    source: &Path,
    destination: &Path,
    relative: &Path,
    digest: &mut InputDigest,
    deadline: Instant,
) -> anyhow::Result<()> {
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
        digest.field(b"directory");
        digest.field(relative.as_os_str().as_bytes());
        let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy(
                &entry.path(),
                &destination.join(entry.file_name()),
                &relative.join(entry.file_name()),
                digest,
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
        let mut contents = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            remaining(deadline)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            contents.update(&buffer[..count]);
        }
        digest.field(b"file");
        digest.field(relative.as_os_str().as_bytes());
        digest.field(&contents.finalize());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_digest_v1_yaml_only_test_vector() {
        assert_eq!(
            InputDigest::new(b"version: 1\n").finish(),
            "v1:sha256:3722640c257aae8ec8b1a29aa32c194764f627f57cdf4d8b143a2a64dbf8f911"
        );
        assert_ne!(
            InputDigest::new(b"version: 1\n").finish(),
            InputDigest::new(b"version: 1").finish()
        );
    }
}
