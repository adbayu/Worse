//! Error type of the Work crate.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum WorkError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: invalid JSON: {message}")]
    Json { path: PathBuf, message: String },
    #[error("{0}")]
    Invalid(String),
    #[error("render failed: {0}")]
    Render(String),
    #[error("sheet engine: {0}")]
    Sheet(String),
    #[error("export failed: {0}")]
    Export(String),
    #[error("import failed: {0}")]
    Import(String),
}

pub type WorkResult<T> = Result<T, WorkError>;

impl WorkError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Reads a file to a string with the path in the error.
pub(crate) fn read_text(path: &std::path::Path) -> WorkResult<String> {
    std::fs::read_to_string(path).map_err(|e| WorkError::io(path, e))
}

/// Writes a file atomically enough for the single writer: temp file in the same folder, then rename.
pub(crate) fn write_file(path: &std::path::Path, bytes: &[u8]) -> WorkResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| WorkError::io(dir, e))?;
    }
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, bytes).map_err(|e| WorkError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| WorkError::io(path, e))
}
