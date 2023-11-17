use std::{error::Error, fmt};

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    Io(std::io::Error),
    Parse(csv::Error),
    Regex(regex::Error),
    Sqlite(rusqlite::Error),
    // IllegalOperation(String),
    Other,
}

pub fn log(msg: String) {
    if cfg!(debug_assertions) {
        let limit = 300;
        if msg.len() > limit {
            eprintln!(
                "{}\n...\n{}",
                &msg[..limit / 2],
                &msg[msg.len() - limit / 2..]
            );
        } else {
            eprintln!("{}", msg);
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Io(err) => write!(f, "IO error: {}", err),
            AppError::Parse(err) => write!(f, "Csv parsing error: {}", err),
            AppError::Regex(err) => write!(f, "Regex parsing error: {}", err),
            AppError::Sqlite(err) => write!(f, "Sqlite error: {}", err),
            // AppError::IllegalOperation(err) => write!(f, "Illegal operation error: {}", err),
            AppError::Other => {
                write!(f, "Other error")
            }
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
            // AppError::IllegalOperation(s) => Some(&AppError::IllegalOperation(s.clone())),
            AppError::Other => Some(self),
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
