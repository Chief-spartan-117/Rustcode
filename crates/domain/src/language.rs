use std::sync::Arc;

/// Language identifier such as `rust` or `python` (VS Code language ids).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Language(Arc<str>);

impl Language {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(Arc::from(id.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
