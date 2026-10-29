use std::path::Path;

use crate::{error::AppResult, model::database::Database};

/// Export the selected table to CSV.
pub fn csv(database: &Database, table_name: &str, path: &Path) -> AppResult<()> {
    database.export_delimited(table_name, path, b',')
}

/// Export the selected table to TSV.
pub fn tsv(database: &Database, table_name: &str, path: &Path) -> AppResult<()> {
    database.export_delimited(table_name, path, b'\t')
}

/// Save the complete SQLite dataset to a database file.
pub fn sqlite(database: &Database, path: &Path) -> AppResult<()> {
    database.backup_db(path)
}
