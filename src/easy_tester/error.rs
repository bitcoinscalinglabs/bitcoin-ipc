use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum EasyTesterError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("parse error at {path}:{line}: {message}")]
    Parse {
        path: PathBuf,
        line: usize,
        message: String,
        text: String,
    },

    #[error("runtime error: {0}")]
    Runtime(String),
}

impl EasyTesterError {
    pub fn parse(
        path: impl Into<PathBuf>,
        line: usize,
        message: impl Into<String>,
        text: &str,
    ) -> Self {
        Self::Parse {
            path: path.into(),
            line,
            message: message.into(),
            text: text.to_string(),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime(message.into())
    }
}
