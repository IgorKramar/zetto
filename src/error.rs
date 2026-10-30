use crate::note::Diagnostic;
use serde_json::Value;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct Error {
    pub code: String,
    pub message: String,
    pub exit: u8,
    pub saved: bool,
    pub data: Option<Box<Value>>,
    pub diagnostics: Vec<Diagnostic>,
    pub recovery: Vec<String>,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let exit = match code {
            "usage" | "invalid_config" | "interaction_required" => 2,
            "conflict" | "lock_busy" | "duplicate_id" | "target_exists" => 3,
            "io" | "editor_failed" | "recovery_required" | "index_unavailable" => 4,
            "interrupted" => 130,
            _ => 1,
        };
        Self {
            code: code.into(),
            message: message.into(),
            exit,
            saved: false,
            data: None,
            diagnostics: Vec::new(),
            recovery: Vec::new(),
        }
    }

    pub fn with_recovery(mut self, paths: Vec<String>) -> Self {
        self.recovery = paths;
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}
