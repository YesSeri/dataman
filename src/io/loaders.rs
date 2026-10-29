use std::path::PathBuf;

use rusqlite::Connection;

use crate::{
    error::{AppError, AppResult},
    model::database::Database,
};

/// Import a CSV file as a table in the supplied SQLite connection.
pub fn csv(path: PathBuf, connection: &Connection) -> AppResult<()> {
    super::csv_format::insert_csv_data_database(path, connection)
}

pub fn tsv(path: PathBuf, connection: &Connection) -> AppResult<()> {
    super::csv_format::insert_tsv_data_database(path, connection)
}

/// Create a dataset from one CSV file and a fresh SQLite connection.
pub fn csv_dataset(path: PathBuf, connection: Connection) -> AppResult<Database> {
    super::csv_format::database_from_csv(path, connection)
}

/// Open an existing SQLite database as a Dataman dataset.
pub fn sqlite(connection: Connection) -> AppResult<Database> {
    super::csv_format::database_from_sqlite(connection)
}

impl TryFrom<Vec<PathBuf>> for Database {
    type Error = AppError;

    fn try_from(paths: Vec<PathBuf>) -> AppResult<Self> {
        Self::load_cancellable(
            paths,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    }
}

impl Database {
    pub(crate) fn load_cancellable(
        paths: Vec<PathBuf>,
        token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> AppResult<Self> {
        let context = format!(
            "Loading {}",
            paths
                .iter()
                .map(|path| format!("'{}'", path.display()))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let cancellation = token.clone();
        (|| {
        if paths.is_empty() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No file paths provided",
            )));
        }
        if paths.len() == 1
            && paths[0]
                .extension()
                .is_some_and(|ext| ext == "sqlite" || ext == "sqlite3" || ext == "db")
        {
            let connection = Connection::open(&paths[0])?;
            Self::install_cancellation(&connection, token.clone());
            let mut database = Self::new(connection)?;
            Self::install_cancellation(&database.connection, token.clone());
            database.cancellation = token;
            return Ok(database);
        }
        if paths.iter().all(|path| {
            path.extension()
                .is_some_and(|ext| ext == "csv" || ext == "tsv")
        }) {
            let directory = tempfile::Builder::new().prefix("dataman-").tempdir()?;
            let connection = Connection::open(directory.path().join("session.sqlite"))?;
            let mut database = Database::new(connection)?;
            database.session_directory = Some(directory);
            database.cancellation = token.clone();
            Self::install_cancellation(&database.connection, token.clone());
            database.atomic(|| {
                for path in paths {
                    database.check_cancelled()?;
                    let delimiter = if path.extension().is_some_and(|ext| ext == "tsv") {
                        b'\t'
                    } else {
                        b','
                    };
                    super::csv_format::insert_delimited_cancellable(
                        path,
                        &database.connection,
                        delimiter,
                        Some(&token),
                    )?;
                }
                Ok(())
            })?;
            return Ok(database);
        }

        Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid file extension. One or several csv files or a single sqlite3 database can be provided.",
        )))
        })().map_err(|error: AppError| error.normalize_cancelled(cancellation.load(std::sync::atomic::Ordering::Relaxed)).context(context))
    }
}
