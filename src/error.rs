use std::error::Error as StdError;
use std::fmt;

pub type Result<T> = std::result::Result<T, ManagerError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerError {
    pub code: &'static str,
    pub message: String,
}

impl ManagerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn io(context: &str, error: std::io::Error) -> Self {
        Self::new("io_error", format!("{context}: {error}"))
    }
}

impl fmt::Display for ManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl StdError for ManagerError {}
