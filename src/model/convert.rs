use rusqlite::Connection;
use std::path::Path;

use crate::error::AppResult;

pub trait Importable {
    fn import_to_db(&self, path: &Path, connection: &Connection) -> AppResult<()>;
}
pub trait Exportable {
    fn export_to_db(&self, path: &Path, connection: &Connection) -> AppResult<()>;
}
