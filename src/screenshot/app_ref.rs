use anyhow::ensure;

#[derive(Debug, Clone)]
pub struct AppRef {
    id: String,
    arch: String,
    branch: String,
}

impl AppRef {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let parts: Vec<_> = text.split('/').collect();
        ensure!(
            parts.len() == 4
                && parts[0] == "app"
                && parts[1..].iter().all(|part| !part.is_empty()
                    && !part.starts_with('-')
                    && !matches!(*part, "." | "..")
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))),
            "expected app/ID/ARCH/BRANCH ref"
        );
        ensure!(
            parts[1].split('.').count() >= 3 && parts[1].split('.').all(|part| !part.is_empty()),
            "invalid application ID"
        );
        Ok(Self {
            id: parts[1].into(),
            arch: parts[2].into(),
            branch: parts[3].into(),
        })
    }

    pub fn from_installed(text: &str) -> anyhow::Result<Self> {
        if text.starts_with("app/") {
            Self::parse(text)
        } else {
            Self::parse(&format!("app/{text}"))
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn arch(&self) -> &str {
        &self.arch
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

impl std::fmt::Display for AppRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "app/{}/{}/{}", self.id, self.arch, self.branch)
    }
}
