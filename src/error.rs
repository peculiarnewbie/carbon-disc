use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not launch {program}: {source}")]
    Launch {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{program} failed: {message}")]
    Process { program: String, message: String },
    #[error("invalid JSON data: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("invalid preview frame: {0}")]
    InvalidPreview(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode was cancelled")]
    Cancelled,
    #[error("output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

pub type Result<T> = std::result::Result<T, Error>;
