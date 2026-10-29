use std::error::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Parse(#[from] csv::Error),
    #[error("{0}")]
    Regex(#[from] regex::Error),
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Operation cancelled")]
    Cancelled,
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Message(String),
    #[error("{message}: {source}")]
    Context {
        message: String,
        #[source]
        source: Box<AppError>,
    },
    #[error("{source}; cleanup also failed: {cleanup}")]
    Cleanup {
        #[source]
        source: Box<AppError>,
        cleanup: Box<AppError>,
    },
}

impl AppError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }
    pub fn context(self, message: impl Into<String>) -> Self {
        Self::Context {
            message: message.into(),
            source: Box::new(self),
        }
    }
    pub fn with_cleanup(self, cleanup: Self) -> Self {
        Self::Cleanup {
            source: Box::new(self),
            cleanup: Box::new(cleanup),
        }
    }
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled => true,
            Self::Context { source, .. } | Self::Cleanup { source, .. } => source.is_cancelled(),
            _ => false,
        }
    }
    pub fn normalize_cancelled(self, cancelled: bool) -> Self {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(code, _))
                if cancelled && code.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                Self::Cancelled
            }
            Self::Context { message, source } => {
                source.normalize_cancelled(cancelled).context(message)
            }
            Self::Cleanup { source, cleanup } => {
                source.normalize_cancelled(cancelled).with_cleanup(*cleanup)
            }
            error => error,
        }
    }
    fn has_cleanup_failure(&self) -> bool {
        match self {
            Self::Cleanup { .. } => true,
            Self::Context { source, .. } => source.has_cleanup_failure(),
            _ => false,
        }
    }

    fn concise(&self) -> String {
        match self {
            Self::Context { message, source } => format!("{message}: {}", source.concise()),
            Self::Cleanup { source, .. } => format!(
                "{}; cleanup also failed (see diagnostics)",
                source.concise()
            ),
            Self::Regex(error) => error
                .to_string()
                .lines()
                .last()
                .unwrap_or("Invalid regex")
                .to_owned(),
            _ => self.to_string(),
        }
    }

    pub(crate) fn present(&self) -> String {
        if self.is_cancelled() && !self.has_cleanup_failure() {
            return "Operation cancelled".into();
        }
        let mut diagnostic = self.to_string();
        let mut cause = self.source();
        while let Some(error) = cause {
            diagnostic.push_str(&format!("\nCaused by: {error}"));
            cause = error.source();
        }
        log::error!("{diagnostic}");
        if self.is_cancelled() {
            "Operation cancelled; cleanup also failed (see diagnostics)".into()
        } else {
            self.concise()
        }
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}

/// Preserve the operation as the primary cause when cleanup also fails.
pub fn finish<T>(result: AppResult<T>, cleanup: AppResult<()>) -> AppResult<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(error.with_cleanup(cleanup)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_terminate_and_preserve_context_and_original_type() {
        for error in [
            AppError::Cancelled,
            AppError::invalid_input("bad input"),
            AppError::from("message"),
        ] {
            assert!(error.source().is_none());
        }
        let error =
            AppError::from(std::io::Error::other("disk failure")).context("Importing data.csv");
        assert!(error.to_string().contains("Importing data.csv"));
        let source = error.source().unwrap();
        assert_eq!(source.to_string(), "disk failure");
        let io = source
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(io.to_string(), "disk failure");
        assert!(io.source().is_none());
    }

    #[test]
    fn operation_remains_primary_when_cleanup_also_fails() {
        let error = finish::<()>(
            Err(AppError::invalid_input("operation")),
            Err(AppError::from("rollback")),
        )
        .unwrap_err();
        assert_eq!(error.source().unwrap().to_string(), "operation");
        assert!(error.to_string().contains("rollback"));
    }

    #[test]
    fn only_requested_sqlite_interruptions_become_cancellation() {
        let interrupted = || {
            AppError::from(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
                None,
            ))
        };
        assert!(interrupted()
            .context("query")
            .normalize_cancelled(true)
            .is_cancelled());
        assert!(!interrupted().normalize_cancelled(false).is_cancelled());
        assert!(!AppError::Sqlite(rusqlite::Error::InvalidQuery)
            .normalize_cancelled(true)
            .is_cancelled());
    }
    #[test]
    fn cancellation_and_recoverable_messages_are_concise() {
        assert_eq!(
            AppError::Cancelled.context("Importing data.csv").present(),
            "Operation cancelled"
        );
        assert_eq!(
            AppError::invalid_input("No rows selected").present(),
            "No rows selected"
        );
        assert_eq!(
            AppError::from(regex::Regex::new("[").unwrap_err()).present(),
            "error: unclosed character class"
        );
    }
}
