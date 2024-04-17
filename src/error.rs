use crate::CONFIG;
use std::{error::Error, fmt};

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    Io(std::io::Error),
    Parse(csv::Error),
    Regex(regex::Error),
    Sqlite(rusqlite::Error),
    Other(Option<String>),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Io(err) => write!(f, "IO error: {}", err),
            AppError::Parse(err) => write!(f, "Csv parsing error: {}", err),
            AppError::Regex(err) => write!(f, "Regex parsing error: {}", err),
            AppError::Sqlite(err) => write!(f, "Sqlite error: {}", err),
            AppError::Other(Some(msg)) => write!(f, "Other error: {}", msg),
            AppError::Other(None) => write!(f, "Other error: no msg"),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            AppError::Io(err) => Some(err),
            AppError::Parse(err) => Some(err),
            AppError::Regex(err) => Some(err),
            AppError::Sqlite(err) => Some(err),
            AppError::Other(_) => Some(self),
        }
    }
}
impl From<csv::Error> for AppError {
    fn from(err: csv::Error) -> AppError {
        AppError::Parse(err)
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> AppError {
        AppError::Io(err)
    }
}

impl From<regex::Error> for AppError {
    fn from(err: regex::Error) -> AppError {
        AppError::Regex(err)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> AppError {
        AppError::Sqlite(err)
    }
}

#[macro_export]
macro_rules! app_error_other {
    ($expression:expr) => {
        if ($expression.is_empty()) {
            AppError::Other(None)
        } else {
            AppError::Other(Some($expression.to_owned()))
        }
    };
}
