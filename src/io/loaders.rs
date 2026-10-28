use std::path::PathBuf;

use rusqlite::Connection;

use crate::{
    app_error_other,
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
        if paths.is_empty() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No file paths provided",
            )));
        }
        if paths.len() == 1 {
            let path = paths[0].clone();
            return match path.extension().and_then(|ext| ext.to_str()) {
                Some("csv") => {
                    let connection = if cfg!(debug_assertions) {
                        Connection::open_in_memory()?
                    } else {
                        let _ = std::fs::remove_file("db.sqlite");
                        Connection::open("db.sqlite")?
                    };
                    csv_dataset(path, connection)
                }
                Some("tsv") => {
                    let connection = Connection::open_in_memory()?;
                    tsv(path, &connection)?;
                    Database::new(connection)
                }
                Some("sqlite") | Some("sqlite3") | Some("db") => sqlite(Connection::open(path)?),
                _ => Err(AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid file extension",
                ))),
            };
        }

        if paths.iter().all(|path| {
            path.extension()
                .is_some_and(|extension| extension == "csv" || extension == "tsv")
        }) {
            let _ = std::fs::remove_file("db.sqlite");
            let connection = Connection::open("db.sqlite")?;
            let first = paths[0].clone();
            let database = if first
                .extension()
                .is_some_and(|extension| extension == "tsv")
            {
                tsv(first, &connection)?;
                Database::new(connection)?
            } else {
                csv_dataset(first, connection)?
            };
            for path in paths.iter().skip(1) {
                if path.extension().is_some_and(|extension| extension == "tsv") {
                    tsv(path.clone(), &database.connection)?;
                } else {
                    csv(path.clone(), &database.connection)?;
                }
            }
            return Ok(database);
        }

        Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid file extension. One or several csv files or a single sqlite3 database can be provided.",
        )))
    }
}
