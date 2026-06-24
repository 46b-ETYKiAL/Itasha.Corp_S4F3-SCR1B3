//! Core error type. Editor operations never panic on user input — they return
//! `CoreError` and the UI surfaces it.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    ConfigParse(String),

    #[error("theme parse error: {0}")]
    ThemeParse(String),

    #[error("invalid regex: {0}")]
    Regex(String),

    // Carries a plugin-subsystem error message verbatim (no category prefix, like
    // `Other`) so every call site that already frames the text — e.g. the command
    // toast `format!("plugin error: {e}")` or the discover line `format!("{path}: {e}")`
    // — renders byte-identically to the pre-A-05 ad-hoc `Result<_, String>`.
    #[error("{0}")]
    Plugin(String),

    #[error("file too large to edit safely ({0} bytes); opened read-only")]
    FileTooLargeToEdit(u64),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
