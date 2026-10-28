use std::path::Path;

use crate::{error::AppResult, model::database::Database};

/// Export the selected table to CSV.
pub fn csv(database: &Database, table_name: &str, path: &Path) -> AppResult<()> {
    super::csv_format::save_to_csv_file(&database.connection, table_name, &path.to_path_buf())
}

/// Export the selected table to TSV.
pub fn tsv(database: &Database, table_name: &str, path: &Path) -> AppResult<()> {
    super::csv_format::save_to_tsv_file(&database.connection, table_name, &path.to_path_buf())
}

/// Save the complete SQLite dataset to a database file.
pub fn sqlite(database: &Database, path: &Path) -> AppResult<()> {
    database.backup_db(path)
}
