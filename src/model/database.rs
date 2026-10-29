use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time;

use rusqlite::{backup, params, types::Value, Connection, Statement};

use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use super::datarow::DataTable;
use super::metadata::{create_table_of_tables, populate_table_of_tables};
use super::regexping;
use super::sql_queries;

pub(crate) const INTERNAL_PREFIX: &str = "__dataman_undo_";

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn data_item_value(value: &DataItem) -> Value {
    match value {
        DataItem::Text(value) => Value::Text(value.clone()),
        DataItem::Integer(value) => Value::Integer(*value),
        DataItem::Float(value) => Value::Real(*value),
        DataItem::Blob(value) => Value::Blob(value.clone()),
        DataItem::Null => Value::Null,
    }
}

#[derive(Debug, Clone)]
pub(crate) enum UndoRecord {
    CellEdit {
        table: String,
        column: String,
        id: i64,
        before: DataItem,
        after: DataItem,
        is_after: bool,
    },
    StoredBulkEdit {
        table: String,
        column: String,
        archive: String,
        is_after: bool,
    },
    StoredDeletedRows {
        table: String,
        archive: String,
        is_deleted: bool,
    },
    RenameTable {
        before: String,
        after: String,
        is_after: bool,
    },
    RenameColumn {
        table: String,
        before: String,
        after: String,
        is_after: bool,
    },
    Visibility {
        table: Option<String>,
        visible_name: String,
        hidden_name: String,
        is_visible: bool,
    },
    Group(Vec<UndoRecord>),
    ViewChange,
}

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
    // Field order matters: close SQLite before removing its directory.
    pub(crate) session_directory: Option<tempfile::TempDir>,
    pub(crate) cancellation: Arc<AtomicBool>,
    cache: RefCell<DatabaseCache>,
    revision: Cell<u64>,
}

#[derive(Debug, Default)]
struct DatabaseCache {
    headers: HashMap<String, Vec<String>>,
    counts: HashMap<String, u32>,
    ordering: Option<(String, String)>,
    names: Option<Vec<String>>,
    storage_keys: HashMap<String, String>,
    value_cache_safe: Option<bool>,
}

impl Database {
    pub fn new(connection: Connection) -> AppResult<Self> {
        Self::purge_internal_artifacts_from(&connection)?;
        create_table_of_tables(&connection)?;
        if let Err(err) = regexping::custom_functions::add_custom_functions(&connection) {
            log::info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            let cancellation = Arc::new(AtomicBool::new(false));
            Self::install_cancellation(&connection, cancellation.clone());
            Ok(Database {
                connection,
                session_directory: None,
                cancellation,
                cache: RefCell::new(DatabaseCache::default()),
                revision: Cell::new(0),
            })
        }
    }
    pub(crate) fn install_cancellation(connection: &Connection, token: Arc<AtomicBool>) {
        connection.progress_handler(1000, Some(move || token.load(Ordering::Relaxed)));
    }

    pub(crate) fn check_cancelled(&self) -> AppResult<()> {
        if self.cancellation.load(Ordering::Relaxed) {
            Err(AppError::from("Operation cancelled"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.get()
    }

    pub(crate) fn invalidate(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
        *self.cache.borrow_mut() = DatabaseCache::default();
    }

    /// Helpers may join an operation's transaction; only the outer owner commits.
    pub(crate) fn atomic<T>(&self, operation: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
        self.atomic_impl(operation, true)
    }

    fn atomic_values<T>(
        &self,
        table: &str,
        column: &str,
        operation: impl FnOnce() -> AppResult<T>,
    ) -> AppResult<T> {
        let cached_safe = self.cache.borrow().value_cache_safe;
        let safe = if let Some(safe) = cached_safe {
            safe
        } else {
            // Triggers and FK cascades can change row counts or other tables.
            let safe = self.connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger') AND NOT EXISTS(SELECT 1 FROM sqlite_master m, pragma_foreign_key_list(m.name) f WHERE m.type='table')", [], |row| row.get::<_,bool>(0)).unwrap_or(false);
            self.cache.borrow_mut().value_cache_safe = Some(safe);
            safe
        };
        if !safe {
            return self.atomic(operation);
        }
        let result = self.atomic_impl(operation, false);
        if result.is_ok() {
            self.revision.set(self.revision.get().wrapping_add(1));
            let mut cache = self.cache.borrow_mut();
            if let Some((ordered_table, ordering)) = &cache.ordering {
                // Preserve the common ID ordering when only cell values change.
                let by_id = [
                    "",
                    "ORDER BY id",
                    "ORDER BY id ASC",
                    "ORDER BY id DESC",
                    "ORDER BY \"id\" ASC",
                    "ORDER BY \"id\" DESC",
                ]
                .contains(&ordering.as_str());
                if ordered_table == table && (!by_id || column == "id") {
                    cache.ordering = None;
                }
            }
        }
        result
    }

    fn atomic_impl<T>(
        &self,
        operation: impl FnOnce() -> AppResult<T>,
        invalidate_all: bool,
    ) -> AppResult<T> {
        if !self.connection.is_autocommit() {
            return operation();
        }
        let transaction = self.connection.unchecked_transaction()?;
        let result = operation().and_then(|value| {
            self.check_cancelled()?;
            Ok(value)
        });
        // COMMIT/ROLLBACK are the operation boundary. A late cancellation must not
        // interrupt rollback or turn a successfully committed mutation into an error.
        self.connection.progress_handler(0, None::<fn() -> bool>);
        let result = match result {
            Ok(value) => transaction.commit().map(|()| value).map_err(AppError::from),
            Err(error) => transaction
                .rollback()
                .map_err(AppError::from)
                .and(Err(error)),
        };
        Self::install_cancellation(&self.connection, self.cancellation.clone());
        if invalidate_all || result.is_err() {
            self.invalidate();
        }
        result
    }

    fn validate_destination(&self, destination: &Path) -> AppResult<()> {
        if let Some(source) = self.connection.path().filter(|path| !path.is_empty()) {
            if let (Ok(source), Ok(destination)) = (
                std::fs::canonicalize(source),
                std::fs::canonicalize(destination),
            ) {
                if source == destination {
                    return Err(AppError::from(
                        "Cannot replace the active SQLite file; save to a different path",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn export_delimited<P: AsRef<Path>>(
        &self,
        table: &str,
        destination: P,
        delimiter: u8,
    ) -> AppResult<()> {
        self.validate_destination(destination.as_ref())?;
        crate::io::csv_format::save_delimited_cancellable(
            &self.connection,
            table,
            &destination.as_ref().to_path_buf(),
            delimiter,
            Some(&self.cancellation),
        )
    }

    pub(crate) fn backup_db<P: AsRef<Path>>(&self, destination: P) -> AppResult<()> {
        let destination = destination.as_ref();
        self.validate_destination(destination)?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut target = Connection::open(temporary.path())?;
            let backup = backup::Backup::new(&self.connection, &mut target)?;
            loop {
                self.check_cancelled()?;
                match backup.step(128)? {
                    backup::StepResult::Done => break,
                    backup::StepResult::More => {}
                    _ => std::thread::sleep(time::Duration::from_millis(10)),
                }
            }
        }
        {
            let target = Connection::open(temporary.path())?;
            Self::install_cancellation(&target, self.cancellation.clone());
            Self::purge_internal_artifacts_from(&target)?;
        }
        self.check_cancelled()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(destination)
            .map_err(|e| AppError::Io(e.error))?;
        Ok(())
    }

    fn storage_key(&self, table: &str) -> String {
        if let Some(key) = self.cache.borrow().storage_keys.get(table) {
            return key.clone();
        }
        let headers = self.get_headers(table).unwrap_or_default();
        for candidate in ["rowid", "_rowid_", "oid"] {
            if !headers
                .iter()
                .any(|name| name.eq_ignore_ascii_case(candidate))
                && self
                    .connection
                    .prepare(&format!(
                        "SELECT {candidate} FROM {} LIMIT 0",
                        quote_identifier(table)
                    ))
                    .is_ok()
            {
                self.cache
                    .borrow_mut()
                    .storage_keys
                    .insert(table.to_owned(), candidate.to_owned());
                return candidate.to_owned();
            }
        }
        "id".to_owned()
    }

    /// Build an indexed, deterministic position map once per table/order revision.
    fn ensure_ordering(&self, table: &str, ordering: &str) -> AppResult<()> {
        let key = (table.to_owned(), ordering.to_owned());
        if self.cache.borrow().ordering.as_ref() == Some(&key) {
            return Ok(());
        }
        let headers = self.get_headers(table)?;
        let identity = if headers.iter().any(|h| h == "id") {
            "id"
        } else {
            "rowid"
        };
        let storage_key = self.storage_key(table);
        let order = if ordering.is_empty() {
            format!("ORDER BY {}", quote_identifier(identity))
        } else {
            format!("{ordering}, {} ASC", quote_identifier(identity))
        };
        self.cache.borrow_mut().ordering = None;
        self.connection.execute_batch("DROP TABLE IF EXISTS temp.__dataman_undo_page_order;
            CREATE TEMP TABLE __dataman_undo_page_order (position INTEGER PRIMARY KEY, row_id INTEGER UNIQUE, storage_key INTEGER);")?;
        self.connection.execute_batch(&format!(
            "INSERT INTO temp.__dataman_undo_page_order SELECT ROW_NUMBER() OVER ({order}) - 1, {}, {} FROM {};",
            quote_identifier(identity), quote_identifier(&storage_key), quote_identifier(table)))?;
        self.cache.borrow_mut().ordering = Some(key);
        Ok(())
    }

    pub fn get(
        &self,
        limit: u32,
        offset: u32,
        table_name: &str,
        ordering: &str,
    ) -> AppResult<DataTable> {
        let headers = self.get_headers(table_name)?;
        self.ensure_ordering(table_name, ordering)?;
        let columns = headers
            .iter()
            .map(|h| format!("t.{}", quote_identifier(h)))
            .collect::<Vec<_>>()
            .join(", ");
        let storage_key = self.storage_key(table_name);
        let query = format!(
            "SELECT {columns} FROM temp.__dataman_undo_page_order p JOIN {} t ON t.{} = p.storage_key \
             WHERE p.position >= {} AND p.position < {} ORDER BY p.position;",
            quote_identifier(table_name),
            quote_identifier(&storage_key),
            offset,
            u64::from(offset) + u64::from(limit)
        );

        let (headers, data_rows) = {
            let mut data_rows = vec![];
            let mut stmt = self.prepare(&query)?;
            let headers: Vec<String> = stmt
                .column_names()
                .into_iter()
                .map(|h| h.to_string())
                .collect();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let mut items = vec![];
                let mut i = 0;
                while let Ok(field) = row.get_ref(i) {
                    items.push(DataItem::from(field));
                    i += 1;
                }
                data_rows.push(items);
            }
            (headers, data_rows)
        };
        Ok((headers, data_rows))
    }
    pub(crate) fn count_rows(&self, table_name: &str) -> Option<u32> {
        if let Some(count) = self.cache.borrow().counts.get(table_name) {
            return Some(*count);
        }
        let count = self
            .connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {};", quote_identifier(table_name)),
                [],
                |row| row.get(0),
            )
            .ok()?;
        self.cache
            .borrow_mut()
            .counts
            .insert(table_name.to_owned(), count);
        Some(count)
    }
    pub fn get_cell(&self, table_name: &str, id: i64, header: &str) -> AppResult<String> {
        Ok(self.get_cell_item(table_name, id, header)?.to_string())
    }

    pub(crate) fn get_cell_item(
        &self,
        table_name: &str,
        id: i64,
        header: &str,
    ) -> AppResult<DataItem> {
        let query = format!(
            "SELECT {} FROM {} WHERE id = ?;",
            quote_identifier(header),
            quote_identifier(table_name)
        );
        let mut stmt = self.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        let row = rows
            .next()?
            .ok_or_else(|| AppError::from("Cell row not found"))?;
        Ok(DataItem::from(row.get_ref(0)?))
    }

    pub(crate) fn toggle_history_record(&self, record: &mut UndoRecord) -> AppResult<()> {
        let original = record.clone();
        let value_target = match record {
            UndoRecord::CellEdit { table, column, .. }
            | UndoRecord::StoredBulkEdit { table, column, .. } => {
                Some((table.clone(), column.clone()))
            }
            _ => None,
        };
        let operation = || {
            match record {
                UndoRecord::CellEdit {
                    table,
                    column,
                    id,
                    before,
                    after,
                    is_after,
                } => {
                    let value = if *is_after { before } else { after };
                    self.update_cell_value(table, column, *id, value)?;
                    *is_after = !*is_after;
                }
                UndoRecord::StoredBulkEdit {
                    table,
                    column,
                    archive,
                    is_after,
                } => {
                    let field = if *is_after {
                        "before_value"
                    } else {
                        "after_value"
                    };
                    self.connection.execute_batch(&format!(
                    "UPDATE {table} SET {column} = (SELECT {field} FROM {archive} a WHERE a.id = {table}.id)
                     WHERE id IN (SELECT id FROM {archive});",
                    table=quote_identifier(table), column=quote_identifier(column), archive=quote_identifier(archive)))?;
                    *is_after = !*is_after;
                }
                UndoRecord::StoredDeletedRows {
                    table,
                    archive,
                    is_deleted,
                } => {
                    if *is_deleted {
                        let archived = self.raw_headers(archive)?;
                        let headers = self
                            .raw_headers(table)?
                            .iter()
                            .filter(|h| archived.contains(h))
                            .map(|h| quote_identifier(h))
                            .collect::<Vec<_>>()
                            .join(",");
                        self.connection.execute_batch(&format!(
                            "INSERT INTO {} ({headers}) SELECT {headers} FROM {};",
                            quote_identifier(table),
                            quote_identifier(archive)
                        ))?;
                    } else {
                        self.connection.execute_batch(&format!(
                            "DELETE FROM {} WHERE id IN (SELECT id FROM {});",
                            quote_identifier(table),
                            quote_identifier(archive)
                        ))?;
                    }
                    *is_deleted = !*is_deleted;
                }
                UndoRecord::RenameTable {
                    before,
                    after,
                    is_after,
                } => {
                    let (from, to) = if *is_after {
                        (after, before)
                    } else {
                        (before, after)
                    };
                    self.rename_table(from, to)?;
                    *is_after = !*is_after;
                }
                UndoRecord::RenameColumn {
                    table,
                    before,
                    after,
                    is_after,
                } => {
                    let (from, to) = if *is_after {
                        (after, before)
                    } else {
                        (before, after)
                    };
                    self.rename_column(table, from, to)?;
                    *is_after = !*is_after;
                }
                UndoRecord::Visibility {
                    table,
                    visible_name,
                    hidden_name,
                    is_visible,
                } => {
                    let (from, to) = if *is_visible {
                        // A replayed command can create a new undo record while an older
                        // redo record still refers to a reserved name. Allocate at toggle
                        // time as well, so stale records can never collide with a live
                        // column created since the record was captured.
                        if let Some(table) = table {
                            let available_name = self.internal_column_name(table)?;
                            *hidden_name = available_name;
                        }
                        (visible_name, hidden_name)
                    } else {
                        (hidden_name, visible_name)
                    };
                    match table {
                        Some(table) => self.rename_column(table, from, to)?,
                        None => self.rename_table(from, to)?,
                    }
                    *is_visible = !*is_visible;
                }
                UndoRecord::Group(records) => {
                    for record in records {
                        self.toggle_history_record(record)?;
                    }
                }
                UndoRecord::ViewChange => {}
            }
            Ok(())
        };
        let result = match value_target {
            Some((table, column)) => self.atomic_values(&table, &column, operation),
            None => self.atomic(operation),
        };
        if result.is_err() {
            *record = original;
        }
        result
    }

    pub(crate) fn internal_column_name(&self, _table: &str) -> AppResult<String> {
        let tables = self.raw_table_names()?;
        let mut headers = Vec::new();
        for table in tables {
            headers.extend(self.raw_headers(&table)?);
        }
        let mut suffix = 0;
        loop {
            let candidate = format!("{INTERNAL_PREFIX}column_{suffix}");
            if !headers.contains(&candidate) {
                return Ok(candidate);
            }
            suffix += 1;
        }
    }

    pub(crate) fn internal_table_name(&self) -> AppResult<String> {
        let names = self.raw_table_names()?;
        let mut suffix = 0;
        loop {
            let candidate = format!("{INTERNAL_PREFIX}table_{suffix}");
            if !names.contains(&candidate) {
                return Ok(candidate);
            }
            suffix += 1;
        }
    }

    pub(crate) fn discard_hidden_record(&self, record: &UndoRecord) -> AppResult<()> {
        if let UndoRecord::Group(records) = record {
            for record in records {
                self.discard_hidden_record(record)?;
            }
            return Ok(());
        }
        if let UndoRecord::StoredBulkEdit { archive, .. }
        | UndoRecord::StoredDeletedRows { archive, .. } = record
        {
            self.connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS {};",
                quote_identifier(archive)
            ))?;
            return Ok(());
        }
        if let UndoRecord::Visibility {
            table,
            hidden_name,
            is_visible: false,
            ..
        } = record
        {
            match table {
                Some(_) => {
                    // A table containing this hidden column may itself be hidden by a later delete.
                    // Internal column names are globally unique, so find it wherever it currently lives.
                    for table in self.raw_table_names()? {
                        if self.raw_headers(&table)?.contains(hidden_name) {
                            self.execute_batch(&format!(
                                "ALTER TABLE {} DROP COLUMN {};",
                                quote_identifier(&table),
                                quote_identifier(hidden_name)
                            ))?;
                            break;
                        }
                    }
                }
                None => self.execute_batch(&format!(
                    "DROP TABLE IF EXISTS {};",
                    quote_identifier(hidden_name)
                ))?,
            }
        }
        Ok(())
    }

    fn update_cell_value(
        &self,
        table: &str,
        column: &str,
        id: i64,
        value: &DataItem,
    ) -> AppResult<()> {
        let sql = format!(
            "UPDATE {} SET {} = ? WHERE id = ?;",
            quote_identifier(table),
            quote_identifier(column)
        );
        self.connection
            .execute(&sql, params![data_item_value(value), id])?;
        Ok(())
    }

    fn raw_table_names(&self) -> AppResult<Vec<String>> {
        let mut stmt = self
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY rowid")?;
        let names = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(names)
    }

    fn raw_headers(&self, table: &str) -> AppResult<Vec<String>> {
        let mut stmt = self
            .connection
            .prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))?;
        let headers = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<_, _>>()?;
        Ok(headers)
    }

    fn purge_internal_artifacts<P: AsRef<Path>>(path: P) -> AppResult<()> {
        let conn = Connection::open(path)?;
        Self::purge_internal_artifacts_from(&conn)
    }

    fn purge_internal_artifacts_from(conn: &Connection) -> AppResult<()> {
        let tables = {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
            let values = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            values
        };
        for table in tables {
            if table.starts_with(INTERNAL_PREFIX) {
                conn.execute(&format!("DROP TABLE {}", quote_identifier(&table)), [])?;
                continue;
            }
            let columns = {
                let mut stmt =
                    conn.prepare(&format!("PRAGMA table_info({})", quote_identifier(&table)))?;
                let values = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?;
                values
            };
            for column in columns
                .into_iter()
                .filter(|column| column.starts_with(INTERNAL_PREFIX))
            {
                conn.execute(
                    &format!(
                        "ALTER TABLE {} DROP COLUMN {}",
                        quote_identifier(&table),
                        quote_identifier(&column)
                    ),
                    [],
                )?;
            }
        }
        Ok(())
    }
    fn prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>> {
        log::info!("{sql}");
        self.connection.prepare(sql)
    }
    fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> AppResult<()> {
        log::info!("{sql}");
        self.connection.execute(sql, params)?;
        self.invalidate();
        Ok(())
    }

    pub fn execute_batch(&self, sql: &str) -> AppResult<()> {
        self.atomic(|| {
            self.connection.execute_batch(sql)?;
            Ok(())
        })?;
        self.invalidate();
        Ok(())
    }
    fn load_selection(&self, ids: &std::collections::HashSet<i64>) -> AppResult<()> {
        self.connection.execute_batch("CREATE TEMP TABLE IF NOT EXISTS __dataman_undo_selected(id INTEGER PRIMARY KEY); DELETE FROM temp.__dataman_undo_selected;")?;
        let mut insert = self
            .connection
            .prepare("INSERT INTO temp.__dataman_undo_selected VALUES (?)")?;
        for id in ids {
            self.check_cancelled()?;
            insert.execute([id])?;
        }
        Ok(())
    }

    pub(crate) fn bulk_edit_selected(
        &self,
        table: &str,
        column: &str,
        ids: &std::collections::HashSet<i64>,
        value: &str,
    ) -> AppResult<UndoRecord> {
        let archive = self.internal_table_name()?;
        self.atomic_values(table, column, || {
            self.load_selection(ids)?;
            self.connection.execute(&format!("CREATE TABLE {} AS SELECT t.id, t.{} AS before_value, ? AS after_value FROM {} t JOIN temp.__dataman_undo_selected s ON s.id=t.id",
                quote_identifier(&archive), quote_identifier(column), quote_identifier(table)), [value])?;
            self.connection.execute_batch(&format!("CREATE UNIQUE INDEX {} ON {} (id);",
                quote_identifier(&format!("{archive}_id")), quote_identifier(&archive)))?;
            self.connection.execute(&format!("UPDATE {} SET {} = ? WHERE id IN (SELECT id FROM temp.__dataman_undo_selected)",
                quote_identifier(table), quote_identifier(column)), [value])?;
            Ok(())
        })?;
        Ok(UndoRecord::StoredBulkEdit {
            table: table.into(),
            column: column.into(),
            archive,
            is_after: true,
        })
    }

    pub(crate) fn delete_selected(
        &self,
        table: &str,
        ids: &std::collections::HashSet<i64>,
    ) -> AppResult<UndoRecord> {
        let archive = self.internal_table_name()?;
        self.atomic(|| {
            self.load_selection(ids)?;
            self.connection.execute_batch(&format!(
                "CREATE TABLE {} AS SELECT t.* FROM {} t JOIN temp.__dataman_undo_selected s ON s.id=t.id;
                 DELETE FROM {} WHERE id IN (SELECT id FROM temp.__dataman_undo_selected);",
                quote_identifier(&archive), quote_identifier(table), quote_identifier(table)))?;
            Ok(())
        })?;
        Ok(UndoRecord::StoredDeletedRows {
            table: table.into(),
            archive,
            is_deleted: true,
        })
    }

    pub(crate) fn copy_selected(
        &self,
        source: &str,
        target: &str,
        ordering: &str,
        ids: &std::collections::HashSet<i64>,
    ) -> AppResult<()> {
        self.ensure_ordering(source, ordering)?;
        self.atomic(|| {
            self.load_selection(ids)?;
            let mut info = self.connection.prepare(&format!("PRAGMA table_info({})", quote_identifier(source)))?;
            let columns = info.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                .collect::<Result<Vec<_>, _>>()?.into_iter().filter(|(name,_)| name != "id" && !name.starts_with(INTERNAL_PREFIX)).collect::<Vec<_>>();
            let definitions = columns.iter().map(|(name,kind)| format!("{} {}", quote_identifier(name), quote_identifier(kind))).collect::<Vec<_>>();
            let mut defs = vec!["id INTEGER PRIMARY KEY".to_owned()]; defs.extend(definitions);
            self.connection.execute_batch(&format!("CREATE TABLE {} ({})", quote_identifier(target), defs.join(",")))?;
            if columns.is_empty() {
                self.connection.execute_batch(&format!("INSERT INTO {} (id) SELECT ROW_NUMBER() OVER (ORDER BY p.position) FROM temp.__dataman_undo_page_order p JOIN temp.__dataman_undo_selected s ON s.id=p.row_id", quote_identifier(target)))?;
            } else {
                let names = columns.iter().map(|(name,_)| quote_identifier(name)).collect::<Vec<_>>().join(",");
                let select = columns.iter().map(|(name,_)| format!("t.{}",quote_identifier(name))).collect::<Vec<_>>().join(",");
                self.connection.execute_batch(&format!("INSERT INTO {} ({names}) SELECT {select} FROM temp.__dataman_undo_selected s JOIN {} t ON t.id=s.id JOIN temp.__dataman_undo_page_order p ON p.row_id=t.id ORDER BY p.position",
                    quote_identifier(target), quote_identifier(source)))?;
            }
            Ok(())
        })
    }

    pub(crate) fn select_regex(
        &self,
        table: &str,
        column: &str,
        pattern: &str,
    ) -> AppResult<std::collections::HashSet<i64>> {
        let regex = regex::Regex::new(pattern)?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, {} FROM {}",
            quote_identifier(column),
            quote_identifier(table)
        ))?;
        let mut rows = statement.query([])?;
        let mut ids = std::collections::HashSet::new();
        while let Some(row) = rows.next()? {
            self.check_cancelled()?;
            let item = DataItem::from(row.get_ref(1)?);
            if !item.is_empty() && regex.is_match(&item.to_string()) {
                ids.insert(row.get(0)?);
            }
        }
        Ok(ids)
    }

    pub fn derive_column<F>(
        &self,
        table_name: &str,
        old_column_name: &str,
        new_column_name: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        self.atomic(|| {
            self.connection.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} TEXT;",
                quote_identifier(table_name),
                quote_identifier(new_column_name)
            ))?;
            let mut read = self.connection.prepare(&format!(
                "SELECT id, {} FROM {} WHERE id > ? ORDER BY id LIMIT 256",
                quote_identifier(old_column_name),
                quote_identifier(table_name)
            ))?;
            let mut update = self.connection.prepare(&format!(
                "UPDATE {} SET {} = ? WHERE id = ?",
                quote_identifier(table_name),
                quote_identifier(new_column_name)
            ))?;
            // Include the complete signed SQLite ID range, including i64::MIN.
            let mut after: Option<i64> = None;
            let mut first = self.connection.prepare(&format!(
                "SELECT id, {} FROM {} ORDER BY id LIMIT 256",
                quote_identifier(old_column_name),
                quote_identifier(table_name)
            ))?;
            loop {
                self.check_cancelled()?;
                let mut rows = match after {
                    Some(id) => read.query([id])?,
                    None => first.query([])?,
                };
                let mut batch = Vec::new();
                while let Some(row) = rows.next()? {
                    batch.push((row.get::<_, i64>(0)?, DataItem::from(row.get_ref(1)?)));
                }
                drop(rows);
                if batch.is_empty() {
                    break;
                }
                for (id, item) in batch {
                    let value = if item == DataItem::Null {
                        None
                    } else {
                        fun(item.to_string())
                    };
                    update.execute(params![value, id])?;
                    after = Some(id);
                }
            }
            Ok(())
        })
    }

    pub fn get_table_names(&self) -> AppResult<Vec<String>> {
        if let Some(names) = &self.cache.borrow().names {
            return Ok(names.clone());
        }
        let query = r#"SELECT name FROM sqlite_master WHERE type='table' ORDER BY rowid;"#;
        let mut stmt = self.prepare(query)?;
        let mut rows = stmt.query([])?;
        let mut table_names = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if !name.starts_with(INTERNAL_PREFIX) && !name.starts_with("sqlite_") {
                table_names.push(name);
            }
        }
        self.cache.borrow_mut().names = Some(table_names.clone());
        Ok(table_names)
    }
    fn find_unused_table_name(&self, table_name: &str) -> AppResult<String> {
        let table_names = self.get_table_names()?;
        let mut new_table_name = table_name.to_string();
        let mut suffix = 1;
        while table_names.contains(&new_table_name) {
            new_table_name = format!("{}_{}", table_name, suffix);
            suffix += 1;
        }
        Ok(new_table_name)
    }

    pub(crate) fn frequency_table(&self, table: &str, column: &str) -> AppResult<String> {
        let result = self.find_unused_table_name(&format!("{table}_frequency"))?;
        let source_table = table.replace('\'', "''");
        let source_column = column.replace('\'', "''");
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} AS value, COUNT(*) AS count, 100.0 * COUNT(*) / NULLIF((SELECT COUNT(*) FROM {}), 0) AS percentage, '{}' AS source_table, '{}' AS source_column FROM {} GROUP BY {} ORDER BY count DESC;",
            quote_identifier(&result), quote_identifier(column), quote_identifier(table), source_table, source_column, quote_identifier(table), quote_identifier(column)
        );
        self.execute_batch(&query)?;
        Ok(result)
    }

    pub(crate) fn frequency_table_with_members(
        &self,
        table: &str,
        column: &str,
    ) -> AppResult<(String, String)> {
        let frequency = self.find_unused_table_name(&format!("{table}_frequency"))?;
        let members = self.internal_table_name()?;
        let source_table = table.replace('\'', "''");
        let source_column = column.replace('\'', "''");
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} AS value, COUNT(*) AS count, 100.0 * COUNT(*) / NULLIF((SELECT COUNT(*) FROM {}), 0) AS percentage, '{}' AS source_table, '{}' AS source_column FROM {} GROUP BY {} ORDER BY count DESC; CREATE TABLE {} (group_id INTEGER NOT NULL, source_id INTEGER NOT NULL); INSERT INTO {} (group_id, source_id) SELECT frequency.id, source.id FROM {} AS source JOIN {} AS frequency ON source.{} IS frequency.value;",
            quote_identifier(&frequency),
            quote_identifier(column),
            quote_identifier(table),
            source_table,
            source_column,
            quote_identifier(table),
            quote_identifier(column),
            quote_identifier(&members),
            quote_identifier(&members),
            quote_identifier(table),
            quote_identifier(&frequency),
            quote_identifier(column),
        );
        self.atomic(|| {
            self.connection.execute_batch(&query)?;
            Ok(())
        })?;
        self.invalidate();
        Ok((frequency, members))
    }

    pub(crate) fn frequency_group_members(
        &self,
        membership_table: &str,
        group_id: i64,
    ) -> AppResult<std::collections::HashSet<i64>> {
        let mut statement = self.prepare(&format!(
            "SELECT source_id FROM {} WHERE group_id = ?1",
            quote_identifier(membership_table)
        ))?;
        let members = statement
            .query_map([group_id], |row| row.get(0))?
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        Ok(members)
    }

    pub(crate) fn summary_table(&self, table: &str, column: &str) -> AppResult<String> {
        let result = self.find_unused_table_name(&format!("{table}_summary"))?;
        let query = format!(
            "CREATE TABLE {} AS SELECT 1 AS id, COUNT(*) AS count, COUNT(*) - COUNT({}) AS null_count, MIN({}) AS min, MAX({}) AS max, AVG(CAST({} AS REAL)) AS average FROM {};",
            quote_identifier(&result), quote_identifier(column), quote_identifier(column), quote_identifier(column), quote_identifier(column), quote_identifier(table)
        );
        self.execute_batch(&query)?;
        Ok(result)
    }

    pub(crate) fn describe_table(&self, table: &str) -> AppResult<String> {
        let result = self.find_unused_table_name(&format!("{table}_describe"))?;
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))?;
        let columns = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(name, _)| !name.starts_with(INTERNAL_PREFIX))
            .collect::<Vec<_>>();
        if columns.is_empty() {
            return Err(AppError::from("Cannot describe a table without columns"));
        }
        let total_table = quote_identifier(table);
        let rows = columns
            .iter()
            .map(|(name, declared_type)| {
                let column = quote_identifier(name);
                let numeric = {
                    let kind = declared_type.to_ascii_uppercase();
                    kind.contains("INT") || kind.contains("REAL") || kind.contains("FLOA") || kind.contains("DOUB") || kind.contains("NUM") || kind.contains("DEC")
                };
                let invalid = if numeric {
                    format!("SUM(CASE WHEN {column} IS NOT NULL AND typeof({column}) NOT IN ('integer','real') THEN 1 ELSE 0 END)")
                } else {
                    "0".into()
                };
                let ordered = if numeric {
                    format!("CASE WHEN typeof({column}) IN ('integer','real') THEN {column} END")
                } else {
                    column.clone()
                };
                let sum = if numeric { format!("SUM({ordered})") } else { "NULL".into() };
                let average = if numeric { format!("AVG({ordered})") } else { "NULL".into() };
                let column_name = name.replace('\'', "''");
                let kind = if declared_type.is_empty() { "BLOB" } else { declared_type.as_str() }.replace('\'', "''");
                format!(
                    "SELECT '{column_name}' AS column_name, '{kind}' AS storage_type, COUNT(*) AS total, COUNT(*) - COUNT({column}) AS null_count, COALESCE({invalid},0) AS invalid_count, COUNT(DISTINCT {column}) AS distinct_count, MIN({ordered}) AS min, MAX({ordered}) AS max, {sum} AS sum, {average} AS average FROM {total_table}"
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, described.* FROM ({rows}) AS described;",
            quote_identifier(&result)
        );
        self.execute_batch(&query)?;
        Ok(result)
    }

    pub(crate) fn join_tables(
        &self,
        left: &str,
        left_column: &str,
        right: &str,
        right_column: &str,
    ) -> AppResult<String> {
        let left_headers = self.get_headers(left)?;
        let right_headers = self.get_headers(right)?;
        if !left_headers.iter().any(|header| header == left_column)
            || !right_headers.iter().any(|header| header == right_column)
        {
            return Err(crate::app_error_other!("Join column does not exist"));
        }
        let result = self.find_unused_table_name(&format!("{left}_{right}_join"))?;
        let mut columns = Vec::new();
        for (source, alias, headers) in [
            (left, "left", left_headers),
            (right, "right", right_headers),
        ] {
            for header in headers.into_iter().filter(|header| header != "id") {
                columns.push(format!(
                    "{}.{} AS {}",
                    quote_identifier(source),
                    quote_identifier(&header),
                    quote_identifier(&format!("{alias}_{header}"))
                ));
            }
        }
        if columns.is_empty() {
            columns.push("1 AS value".into());
        }
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} FROM {} INNER JOIN {} ON {}.{} = {}.{};",
            quote_identifier(&result), columns.join(", "), quote_identifier(left), quote_identifier(right),
            quote_identifier(left), quote_identifier(left_column), quote_identifier(right), quote_identifier(right_column)
        );
        self.execute_batch(&query)?;
        Ok(result)
    }

    fn find_unused_header_name(&self, table_name: &str, header_name: &str) -> AppResult<String> {
        let header_names = self.get_headers(table_name)?;
        let mut new_header_name = header_name.to_string();
        let mut suffix = 1;
        while header_names.contains(&new_header_name) {
            new_header_name = format!("{}_{}", header_name, suffix);
            suffix += 1;
        }
        // log everything
        Ok(new_header_name)
    }
    pub fn regex_filter(&self, table_name: &str, header: &str, pattern: &str) -> AppResult<String> {
        // create new table with filter applied using create table as sqlite statement.
        let old_table_name = table_name;
        let temp_table_name = format!("{}_filt", old_table_name);
        let new_table_name = self.find_unused_table_name(&temp_table_name)?;
        let (query, new_table_name) =
            regexping::regex_filter_query(header, pattern, old_table_name, &new_table_name)?;

        let res = self.execute(&query, []);
        res?;
        Ok(new_table_name)
    }

    // go to first match
    pub fn exact_search(
        &self,
        table_name: &str,
        ordering: &str,
        current_row: u32,
        search_header: &str,
        pattern: &str,
    ) -> AppResult<u32> {
        self.ensure_ordering(table_name, ordering)?;
        let position: u32 = self.connection.query_row(&format!(
            "SELECT p.position FROM temp.__dataman_undo_page_order p JOIN {} t ON t.{} = p.storage_key
             WHERE t.{} = ? AND p.position > ? ORDER BY p.position LIMIT 1",
            quote_identifier(table_name), quote_identifier(&self.storage_key(table_name)), quote_identifier(search_header)), params![pattern, current_row], |r| r.get(0))?;
        Ok(position)
    }

    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.
    pub(crate) fn regex_capture_group_transform(
        &self,
        table_name: &str,
        pattern: &str,
        header: &str,
        transformation: &str,
    ) -> AppResult<()> {
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        let queries = regexping::regex_with_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            transformation,
            table_name,
        )?;
        self.execute_batch(&queries)?;
        Ok(())
    }
    pub(crate) fn regex_no_capture_group_transform(
        &self,
        table_name: &str,
        pattern: &str,
        header: &str,
    ) -> AppResult<()> {
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        let queries = regexping::regex_no_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            table_name,
        )?;
        self.execute_batch(&queries)?;
        Ok(())
    }

    pub(crate) fn copy(&self, table_name: &str, header: &str) -> AppResult<()> {
        let transform_header = format!("{}_copy", header);
        let new_header_name = self.find_unused_header_name(table_name, &transform_header)?;
        // let derived_header_name = format!("derived_{}", header);
        let create_header_query =
            format!(r#"ALTER TABLE "{table_name}" ADD COLUMN "{new_header_name}" TEXT;"#);

        let mut queries = String::new();
        queries.push_str(&create_header_query);
        let update_query =
            format!(r#"UPDATE "{table_name}" SET "{new_header_name}" = "{header}";"#);
        queries.push_str(&update_query);
        self.execute_batch(&queries)
    }
    pub(crate) fn sql_query(&self, query: &str) -> AppResult<()> {
        // User transaction statements must not escape the operation's rollback boundary.
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        self.atomic(|| {
            self.connection.authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Transaction { .. } | AuthAction::Savepoint { .. }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
            let result = self.connection.execute_batch(query).map_err(AppError::from);
            self.connection
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            result
        })
    }

    pub(crate) fn get_table_name(file: PathBuf) -> Option<String> {
        file.file_stem().map(|el| el.to_string_lossy().into_owned())
    }

    pub fn get_headers(&self, table_name: &str) -> AppResult<Vec<String>> {
        if let Some(headers) = self.cache.borrow().headers.get(table_name) {
            return Ok(headers.clone());
        }
        let query = format!("PRAGMA table_info({})", quote_identifier(table_name));
        let mut stmt = self.connection.prepare(&query)?;
        let column_names: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .map(|result| result.expect("Failed to retrieve column name"))
            .filter(|name: &String| !name.starts_with(INTERNAL_PREFIX))
            .collect();
        self.cache
            .borrow_mut()
            .headers
            .insert(table_name.to_owned(), column_names.clone());
        Ok(column_names)
    }
    pub fn count_headers(&self, table_name: &str) -> AppResult<u16> {
        Ok(self.get_headers(table_name)?.len() as u16)
    }

    pub fn update_cell(
        &self,
        table_name: &str,
        header: &str,
        id: i64,
        content: &str,
    ) -> AppResult<()> {
        if header == "id" {
            return Err(AppError::from("The id column is read-only"));
        }
        let update_query = format!(
            "UPDATE {} SET {} = ? WHERE id = ?",
            quote_identifier(table_name),
            quote_identifier(header)
        );
        self.atomic_values(table_name, header, || {
            self.connection
                .execute(&update_query, params![content, id])?;
            Ok(())
        })
    }

    pub(crate) fn text_to_int(&self, table_name: &str, column: &str) -> AppResult<()> {
        let derived = self.find_unused_header_name(table_name, &format!("INTEGER_{column}"))?;
        self.atomic(|| {
            self.connection.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} INTEGER; UPDATE {} SET {} = dataman_try_integer({});",
                quote_identifier(table_name),
                quote_identifier(&derived),
                quote_identifier(table_name),
                quote_identifier(&derived),
                quote_identifier(column),
            ))?;
            Ok(())
        })
    }

    pub(crate) fn int_to_text(&self, table_name: &str, column: &str) -> AppResult<()> {
        let queries = sql_queries::build::int_to_text_query(table_name, column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_column(&self, table_name: &str, column: &str) -> AppResult<String> {
        if column == "id" {
            return Err(AppError::from(
                "The id column is required and cannot be deleted",
            ));
        }
        let hidden = self.internal_column_name(table_name)?;
        self.rename_column(table_name, column, &hidden)?;
        Ok(hidden)
    }

    pub(crate) fn rename_column(
        &self,
        table_name: &str,
        column: &str,
        new_column: &str,
    ) -> AppResult<()> {
        if column == "id" || new_column == "id" {
            return Err(AppError::from(
                "The id column is required and cannot be renamed",
            ));
        }
        let queries = sql_queries::build::rename_column_query(table_name, column, new_column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_table(&self, table_name: &str) -> AppResult<String> {
        let hidden = self.internal_table_name()?;
        self.rename_table(table_name, &hidden)?;
        log::info!("Deleted table {table_name}");
        Ok(hidden)
    }

    pub(crate) fn delete_tables(&self, table_names: &[String]) -> AppResult<Vec<(String, String)>> {
        self.atomic(|| {
            let mut deleted = Vec::with_capacity(table_names.len());
            for table in table_names {
                if table == "table_of_tables" || table.starts_with(INTERNAL_PREFIX) {
                    return Err(AppError::from("Protected tables cannot be deleted"));
                }
                let hidden = self.internal_table_name()?;
                self.rename_table(table, &hidden)?;
                deleted.push((table.clone(), hidden));
            }
            Ok(deleted)
        })
    }

    pub(crate) fn rename_table(&self, old_table_name: &str, new_table_name: &str) -> AppResult<()> {
        let query = sql_queries::build::rename_table_query(old_table_name, new_table_name);
        self.execute(&query, [])?;
        Ok(())
    }

    // TODO if the column contains a float, 3.0, then ensure that ALL intermediary calculations are done with floats.
    // currently (3/2)*2.0 = 2.0, but it should be 3.0.
    pub(crate) fn math_operation(&self, table_name: &str, inputs: Vec<String>) -> AppResult<()> {
        let math_expr = inputs[0].clone();
        let new_math_expr_col = self.find_unused_header_name(table_name, "math_expr")?;
        let query =
            sql_queries::build::math_expression_query(&new_math_expr_col, table_name, &math_expr);
        self.execute_batch(&query)
    }

    pub(crate) fn view_metadata_table(&self) -> Result<(), AppError> {
        self.atomic(|| populate_table_of_tables(&self.connection))?;
        self.invalidate();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn setup_database() -> Database {
        Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap()
    }
    fn setup_three_table_db() -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        connection
            .execute("CREATE TABLE t2 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        connection
            .execute("CREATE TABLE t3 (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        Database::new(connection).unwrap()
    }

    #[test]
    fn get_number_of_headers_test() {
        let database = setup_database();
        let number_of_headers = database.count_headers("data").unwrap();
        assert_eq!(number_of_headers, 4)
    }

    #[test]
    fn frequency_table_groups_nulls_and_summary_counts_them() {
        let database = setup_database();
        database.execute_batch("CREATE TABLE sample (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO sample(value) VALUES ('a'), ('a'), (NULL), ('b');").unwrap();
        let frequencies = database.frequency_table("sample", "value").unwrap();
        let rows = database
            .get(10, 0, &frequencies, "ORDER BY value")
            .unwrap()
            .1;
        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .any(|row| row.get(1) == Some(&DataItem::Null)
                && row.get(2) == Some(&DataItem::Integer(1))));
        let repeated = rows
            .iter()
            .find(|row| row.get(1) == Some(&DataItem::Text("a".into())))
            .unwrap();
        assert_eq!(repeated.get(3), Some(&DataItem::Float(50.0)));
        assert_eq!(repeated.get(4), Some(&DataItem::Text("sample".into())));
        assert_eq!(repeated.get(5), Some(&DataItem::Text("value".into())));

        let summary = database.summary_table("sample", "value").unwrap();
        let row = database.get(1, 0, &summary, "").unwrap().1.remove(0);
        assert_eq!(row.get(1), Some(&DataItem::Integer(4)));
        assert_eq!(row.get(2), Some(&DataItem::Integer(1)));
    }

    #[test]
    fn text_to_integer_preserves_source_and_sets_invalid_values_to_null() {
        let database = setup_database();
        database
            .execute_batch("CREATE TABLE typed (id INTEGER PRIMARY KEY, raw TEXT); INSERT INTO typed(raw) VALUES (' 42 '), ('bad'), (''), (NULL), ('-7');")
            .unwrap();
        database.text_to_int("typed", "raw").unwrap();
        assert_eq!(
            database.get_cell_item("typed", 1, "raw").unwrap(),
            DataItem::Text(" 42 ".into())
        );
        assert_eq!(
            database.get_cell_item("typed", 1, "INTEGER_raw").unwrap(),
            DataItem::Integer(42)
        );
        assert_eq!(
            database.get_cell_item("typed", 2, "INTEGER_raw").unwrap(),
            DataItem::Null
        );
        assert_eq!(
            database.get_cell_item("typed", 3, "INTEGER_raw").unwrap(),
            DataItem::Null
        );
        assert_eq!(
            database.get_cell_item("typed", 4, "INTEGER_raw").unwrap(),
            DataItem::Null
        );
        assert_eq!(
            database.get_cell_item("typed", 5, "INTEGER_raw").unwrap(),
            DataItem::Integer(-7)
        );
        database.text_to_int("typed", "raw").unwrap();
        assert!(database
            .get_headers("typed")
            .unwrap()
            .contains(&"INTEGER_raw_1".into()));
    }

    #[test]
    fn describe_uses_all_columns_and_excludes_invalid_numeric_text_from_aggregates() {
        let database = setup_database();
        database
            .execute_batch("CREATE TABLE metrics (id INTEGER PRIMARY KEY, amount INTEGER, label TEXT); INSERT INTO metrics(amount,label) VALUES (4,'z'), (8,'a'), ('bad',NULL);")
            .unwrap();
        let described = database.describe_table("metrics").unwrap();
        let rows = database.get(10, 0, &described, "ORDER BY id").unwrap().1;
        assert_eq!(rows.len(), 3);
        let amount = rows
            .iter()
            .find(|row| row[1] == DataItem::Text("amount".into()))
            .unwrap();
        assert_eq!(amount[3], DataItem::Integer(3));
        assert_eq!(amount[4], DataItem::Integer(0));
        assert_eq!(amount[5], DataItem::Integer(1));
        assert_eq!(amount[6], DataItem::Integer(3));
        assert_eq!(amount[7], DataItem::Integer(4));
        assert_eq!(amount[8], DataItem::Integer(8));
        assert_eq!(amount[9], DataItem::Integer(12));
        assert_eq!(amount[10], DataItem::Float(6.0));
        let label = rows
            .iter()
            .find(|row| row[1] == DataItem::Text("label".into()))
            .unwrap();
        assert_eq!(label[4], DataItem::Integer(1));
        assert_eq!(label[7], DataItem::Text("a".into()));
        assert_eq!(label[8], DataItem::Text("z".into()));
        assert_eq!(label[9], DataItem::Null);
        assert_eq!(label[10], DataItem::Null);
    }

    #[test]
    fn frequency_memberships_keep_duplicate_and_null_group_rows() {
        let database = setup_database();
        database
            .execute_batch("CREATE TABLE sample (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO sample(value) VALUES ('a'), ('a'), (NULL), ('b');")
            .unwrap();
        let (frequency, memberships) = database
            .frequency_table_with_members("sample", "value")
            .unwrap();
        let rows = database.get(10, 0, &frequency, "").unwrap().1;
        for row in rows {
            let group_id = match row[0] {
                DataItem::Integer(id) => id,
                _ => panic!("expected group id"),
            };
            let value = &row[1];
            let expected = match value {
                DataItem::Text(value) if value == "a" => [1, 2].into_iter().collect(),
                DataItem::Text(value) if value == "b" => [4].into_iter().collect(),
                DataItem::Null => [3].into_iter().collect(),
                _ => panic!("unexpected frequency value"),
            };
            assert_eq!(
                database
                    .frequency_group_members(&memberships, group_id)
                    .unwrap(),
                expected
            );
        }
        assert!(!database.get_table_names().unwrap().contains(&memberships));
    }

    #[test]
    fn cancelled_frequency_with_members_rolls_back_both_tables() {
        let database = setup_database();
        database.execute_batch("CREATE TABLE sample (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO sample(value) VALUES ('a'), ('b');").unwrap();
        database
            .cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(database
            .frequency_table_with_members("sample", "value")
            .is_err());
        database
            .cancellation
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let names = database.get_table_names().unwrap();
        assert!(!names
            .iter()
            .any(|name| name.starts_with("sample_frequency")));
        assert!(!names.iter().any(|name| name.starts_with(INTERNAL_PREFIX)));
    }

    #[test]
    fn joins_create_a_separate_result_table() {
        let database = setup_database();
        database.execute_batch("CREATE TABLE lefts (id INTEGER PRIMARY KEY, key TEXT, name TEXT); INSERT INTO lefts(key,name) VALUES ('x','Ada'), ('y','Grace'); CREATE TABLE rights (id INTEGER PRIMARY KEY, key TEXT, score INTEGER); INSERT INTO rights(key,score) VALUES ('x', 9), ('z', 4);").unwrap();
        let result = database
            .join_tables("lefts", "key", "rights", "key")
            .unwrap();
        assert_eq!(database.count_rows(&result), Some(1));
        assert_eq!(database.get_cell(&result, 1, "left_name").unwrap(), "Ada");
        assert_eq!(database.get_cell(&result, 1, "right_score").unwrap(), "9");
        assert!(database
            .get_table_names()
            .unwrap()
            .contains(&"lefts".into()));
        assert!(database
            .get_table_names()
            .unwrap()
            .contains(&"rights".into()));
    }

    #[test]
    fn derive_column_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let col = "firstname";
        let col_new = "firstname_derived";
        let fun = |s| Some(format!("{}-changed", s));
        database.derive_column("data", col, col_new, fun).unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .get(4)
            .unwrap()
            .to_string();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers("data").unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn update_cell() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database
            .update_cell("data", "firstname", 1, "hank")
            .unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .get(1)
            .unwrap()
            .to_string();

        let (_, rows) = database.get(1, 0, "data", "").unwrap();

        // assert_eq!(first, "hank");
    }

    #[test]
    fn update_cell_rejects_the_id_column() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        assert!(database.update_cell("data", "id", 1, "10").is_err());
        assert_eq!(database.get_cell("data", 1, "firstname").unwrap(), "henrik");
    }

    #[test]
    fn get_cell_returns_error_for_missing_row() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        assert!(database.get_cell("data", i64::MAX, "firstname").is_err());
    }

    #[test]
    fn get_cell_formats_integer_values() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, count INTEGER);",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO t (id, count) VALUES (1, 42);", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        assert_eq!(database.get_cell("t", 1, "count").unwrap(), "42");
    }

    #[test]
    fn visibility_history_reallocates_a_stale_internal_column_name() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, value TEXT);", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .execute_batch("ALTER TABLE t ADD COLUMN __dataman_undo_column_0 TEXT;")
            .unwrap();
        let mut record = UndoRecord::Visibility {
            table: Some("t".into()),
            visible_name: "value".into(),
            hidden_name: "__dataman_undo_column_0".into(),
            is_visible: true,
        };

        database.toggle_history_record(&mut record).unwrap();
        assert!(database
            .raw_headers("t")
            .unwrap()
            .contains(&"__dataman_undo_column_1".into()));
        database.toggle_history_record(&mut record).unwrap();
        assert!(database.get_headers("t").unwrap().contains(&"value".into()));
    }

    #[test]
    fn get_table_names_hides_sqlite_system_tables() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);",
                [],
            )
            .unwrap();
        let database = Database::new(connection).unwrap();
        assert!(!database
            .get_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with("sqlite_")));
    }

    #[test]
    fn regex_filter_accepts_apostrophes_in_patterns() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute("INSERT INTO t (name) VALUES ('a''b'), ('abc');", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        let filtered = database.regex_filter("t", "name", "'b").unwrap();
        assert_eq!(database.count_rows(&filtered), Some(1));
    }

    #[test]
    fn regex_filter_treats_null_cells_as_nonmatches() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute("INSERT INTO t (name) VALUES (NULL), ('abc');", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        let filtered = database.regex_filter("t", "name", "a").unwrap();
        assert_eq!(database.count_rows(&filtered), Some(1));
    }

    #[test]
    fn regex_transform_accepts_apostrophes_in_patterns() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute("INSERT INTO t (name) VALUES ('a''b');", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .regex_no_capture_group_transform("t", "'", "name")
            .unwrap();
        assert_eq!(
            database.get_cell_item("t", 1, "name_tform").unwrap(),
            DataItem::Text("'".into())
        );
    }

    #[test]
    fn derive_column_preserves_null_source_cells() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute("INSERT INTO t (name) VALUES (NULL), ('abc');", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .derive_column("t", "name", "derived", |value| Some(value.to_uppercase()))
            .unwrap();
        assert_eq!(
            database.get_cell_item("t", 1, "derived").unwrap(),
            DataItem::Null
        );
    }

    #[test]
    fn derive_column_none_result_is_sql_null() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);", [])
            .unwrap();
        connection
            .execute("INSERT INTO t (name) VALUES ('abc');", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .derive_column("t", "name", "derived", |_| None)
            .unwrap();
        assert_eq!(
            database.get_cell_item("t", 1, "derived").unwrap(),
            DataItem::Null
        );
    }

    #[test]
    fn derive_column_accepts_numeric_source_cells() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, count INTEGER);",
                [],
            )
            .unwrap();
        connection
            .execute("INSERT INTO t (count) VALUES (42);", [])
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .derive_column("t", "count", "derived", Some)
            .unwrap();
        assert_eq!(
            database.get_cell_item("t", 1, "derived").unwrap(),
            DataItem::Text("42".into())
        );
    }

    #[test]
    fn test_offset() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();

        // we use limit 2 so we can check first and second item
        let (headers, data_rows) = database.get(2, 0, "data", "").unwrap();
        let first: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("henrik", first);

        // we can look into the old data rows to see the next item:
        let second: String = data_rows[1].get(1).unwrap().to_string();
        assert_eq!("john", second);

        // or we can update the 'cache'
        // this makes us get a new view into the database
        // if we don't use this we would need to index into [1] instead
        let (headers, data_rows) = database.get(1, 1, "data", "").unwrap();
        let second: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("john", second);
    }

    #[test]
    fn get_table_names_test() {
        let database = setup_database();
        let table_names = database.get_table_names().unwrap();
    }

    #[test]
    fn get_headers_test() {
        let database = setup_database();
        let headers = database.get_headers("data").unwrap();
        assert_eq!(headers, vec!["id", "firstname", "lastname", "age"]);
    }

    #[test]
    fn count_rows_test() {
        let database = setup_database();
        let rows_len = database.count_rows("data").unwrap();
        assert_eq!(rows_len, 6)
    }

    #[test]
    fn custom_functions_regexp_test() {
        let database = setup_database();
        let query = r#"SELECT firstname FROM "data" WHERE regexp('h.*k', firstname)"#;
        let result: String = database
            .connection
            .query_row(query, [], |row| row.get(0))
            .unwrap();
        assert_eq!(result, "henrik");
    }
    #[test]
    fn table_of_tables_test() {
        let database = setup_three_table_db();
    }

    #[test]
    fn copy_column_long_test() {
        let p = vec![PathBuf::from("assets/data-long.csv")];
        let database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        let table_name = "data-long";
        let column_name = "first-name";
        let new_column_name = database
            .find_unused_header_name(table_name, column_name)
            .unwrap();
        database
            .derive_column(table_name, column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 0, table_name, "").unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_test() {
        let p = vec![PathBuf::from("assets/data.csv")];
        let database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        let column_name = "firstname";
        let new_column_name = database
            .find_unused_header_name("data", column_name)
            .unwrap();
        database
            .derive_column("data", column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 100, "data", "").unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }
}

#[cfg(test)]
mod performance_regressions {
    use super::*;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;

    fn database(rows: usize) -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        let database = Database::new(connection).unwrap();
        database
            .atomic(|| {
                let mut insert = database.connection.prepare("INSERT INTO t VALUES (?, ?)")?;
                for id in 1..=rows {
                    insert.execute(params![id as i64, format!("v{}", id % 7)])?;
                }
                Ok(())
            })
            .unwrap();
        database
    }

    #[test]
    fn imported_sessions_are_isolated_and_removed_on_drop() {
        let input = tempfile::tempdir().unwrap();
        let source = input.path().join("data.csv");
        std::fs::write(&source, "value\none\ntwo\n").unwrap();
        let first = Database::try_from(vec![source.clone()]).unwrap();
        let second = Database::try_from(vec![source]).unwrap();
        let first_path = first.session_directory.as_ref().unwrap().path().to_owned();
        let second_path = second.session_directory.as_ref().unwrap().path().to_owned();
        assert_ne!(first_path, second_path);
        first.update_cell("data", "value", 1, "changed").unwrap();
        assert_eq!(second.get_cell("data", 1, "value").unwrap(), "one");
        drop(first);
        assert!(!first_path.exists());
        assert!(second_path.exists());
        drop(second);
        assert!(!second_path.exists());
    }

    #[test]
    fn bulk_edit_is_one_transaction_and_preserves_large_ids_in_history() {
        let database = database(100);
        database
            .execute_batch("INSERT INTO t VALUES (5000000000, 'large')")
            .unwrap();
        let commits = Arc::new(AtomicUsize::new(0));
        let counter = commits.clone();
        database.connection.commit_hook(Some(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            false
        }));
        let ids = (1..=100).chain([5_000_000_000]).collect();
        let mut record = database
            .bulk_edit_selected("t", "value", &ids, "new")
            .unwrap();
        assert_eq!(commits.load(Ordering::Relaxed), 1);
        assert_eq!(
            database.get_cell("t", 5_000_000_000, "value").unwrap(),
            "new"
        );
        database.toggle_history_record(&mut record).unwrap();
        assert_eq!(
            database.get_cell("t", 5_000_000_000, "value").unwrap(),
            "large"
        );
        database.toggle_history_record(&mut record).unwrap();
        assert_eq!(commits.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn failed_bulk_edit_rolls_back_rows_and_archive() {
        let database = database(3);
        database.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE ON t WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
        assert!(database
            .bulk_edit_selected("t", "value", &HashSet::from([1, 2, 3]), "new")
            .is_err());
        assert_eq!(database.get_cell("t", 1, "value").unwrap(), "v1");
        assert!(!database
            .raw_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with(INTERNAL_PREFIX)));
        assert!(database.connection.is_autocommit());
    }

    #[test]
    fn cancellation_mid_operation_rolls_back_and_next_operation_works() {
        let database = database(3);
        let error = database
            .atomic(|| {
                database
                    .connection
                    .execute("UPDATE t SET value='changed'", [])?;
                database.cancellation.store(true, Ordering::Relaxed);
                database.check_cancelled()
            })
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        database.cancellation.store(false, Ordering::Relaxed);
        assert_eq!(database.get_cell("t", 1, "value").unwrap(), "v1");
        database
            .bulk_edit_selected("t", "value", &HashSet::from([1]), "next")
            .unwrap();
        assert_eq!(database.get_cell("t", 1, "value").unwrap(), "next");
    }

    #[test]
    fn cancelled_undo_restores_record_state_and_data() {
        let database = database(3);
        let mut record = database
            .delete_selected("t", &HashSet::from([1, 2]))
            .unwrap();
        database.cancellation.store(true, Ordering::Relaxed);
        assert!(database.toggle_history_record(&mut record).is_err());
        database.cancellation.store(false, Ordering::Relaxed);
        assert!(matches!(
            record,
            UndoRecord::StoredDeletedRows {
                is_deleted: true,
                ..
            }
        ));
        assert_eq!(database.count_rows("t"), Some(1));
        database.toggle_history_record(&mut record).unwrap();
        assert_eq!(database.count_rows("t"), Some(3));
    }

    #[test]
    fn regex_selection_and_selected_copy_reach_beyond_one_million_rows() {
        let database = database(0);
        database.execute_batch("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<1000001) INSERT INTO t SELECT id, CASE WHEN id=1000001 THEN 'last' ELSE 'other' END FROM n;").unwrap();
        let selected = database.select_regex("t", "value", "^last$").unwrap();
        assert_eq!(selected, HashSet::from([1_000_001]));
        database
            .copy_selected("t", "copied", "ORDER BY id", &selected)
            .unwrap();
        assert_eq!(database.get_cell("copied", 1, "value").unwrap(), "last");
        let mut record = database.delete_selected("t", &selected).unwrap();
        assert!(database.get_cell("t", 1_000_001, "value").is_err());
        database.toggle_history_record(&mut record).unwrap();
        assert_eq!(database.get_cell("t", 1_000_001, "value").unwrap(), "last");
    }

    #[test]
    fn ordered_pages_are_stable_for_nulls_duplicates_and_ctas_tables() {
        let database = database(0);
        database.execute_batch("INSERT INTO t VALUES (3,'same'),(1,NULL),(2,'same'),(4,'z'); CREATE TABLE derived AS SELECT * FROM t;").unwrap();
        for table in ["t", "derived"] {
            let ids = |order: &str| {
                (0..4)
                    .map(|offset| database.get(1, offset, table, order).unwrap().1[0][0].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids("ORDER BY value ASC"),
                [1, 2, 3, 4].map(DataItem::Integer)
            );
            assert_eq!(
                ids("ORDER BY value DESC"),
                [4, 2, 3, 1].map(DataItem::Integer)
            );
            assert_eq!(
                database
                    .exact_search(table, "ORDER BY value ASC", 1, "value", "same")
                    .unwrap(),
                2
            );
        }
        // The derived table has no id index: page retrieval must use its storage rowid.
        let plan: String = database.connection.query_row("EXPLAIN QUERY PLAN SELECT t.* FROM temp.__dataman_undo_page_order p JOIN derived t ON t.rowid=p.storage_key WHERE p.position>=2 AND p.position<3 ORDER BY p.position", [], |r| r.get(3)).unwrap();
        assert!(plan.contains("INTEGER PRIMARY KEY"), "{plan}");
    }

    #[test]
    fn cancelled_order_rebuild_does_not_reuse_an_incomplete_map() {
        let database = database(1000);
        database.get(10, 0, "t", "ORDER BY id").unwrap();
        database.cancellation.store(true, Ordering::Relaxed);
        assert!(database.get(10, 0, "t", "ORDER BY value").is_err());
        database.cancellation.store(false, Ordering::Relaxed);
        let page = database.get(10, 0, "t", "ORDER BY id").unwrap();
        assert_eq!(page.1.len(), 10);
        assert_eq!(page.1[0][0], DataItem::Integer(1));
    }

    #[test]
    fn unchanged_page_and_counts_issue_no_queries() {
        let mut controller = crate::controller::controller_impl::Controller::new(database(10));
        controller.get_headers_and_rows(100).unwrap();
        controller.database.count_rows("t").unwrap();
        let queries = Arc::new(AtomicUsize::new(0));
        let counter = queries.clone();
        controller
            .database
            .connection
            .authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Select | AuthAction::Pragma { .. }
                ) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }));
        controller.get_headers_and_rows(100).unwrap();
        let _ = crate::controller::worker::RenderState::from_controller(&controller);
        assert_eq!(queries.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancelled_exports_and_backups_preserve_existing_destinations() {
        let database = database(1000);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.csv");
        let backup = directory.path().join("backup.db");
        std::fs::write(&output, b"original").unwrap();
        std::fs::write(&backup, b"original").unwrap();
        database.cancellation.store(true, Ordering::Relaxed);
        assert!(database.export_delimited("t", &output, b',').is_err());
        assert!(database.backup_db(&backup).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"original");
        assert_eq!(std::fs::read(backup).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn malformed_import_rolls_back_table_and_previous_rows() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("broken.csv");
        std::fs::write(&input, "a,b\none,two\nwrong\n").unwrap();
        let connection = Connection::open_in_memory().unwrap();
        assert!(crate::io::loaders::csv(input, &connection).is_err());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name='broken'",
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_delete_rolls_back_selected_rows_and_history_archive() {
        let database = database(3);
        database.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON t WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
        assert!(database
            .delete_selected("t", &HashSet::from([1, 2, 3]))
            .is_err());
        assert_eq!(database.count_rows("t"), Some(3));
        assert!(!database
            .raw_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with(INTERNAL_PREFIX)));
    }

    #[test]
    fn cancelled_derivation_rolls_back_the_added_column() {
        let database = database(10);
        let cancellation = database.cancellation.clone();
        assert!(database
            .derive_column("t", "value", "derived", move |value| {
                cancellation.store(true, Ordering::Relaxed);
                Some(value)
            })
            .is_err());
        database.cancellation.store(false, Ordering::Relaxed);
        assert_eq!(database.get_headers("t").unwrap(), ["id", "value"]);
    }

    #[test]
    fn mid_import_cancellation_rolls_back_inserted_rows_and_schema() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("cancelled.csv");
        let mut contents = String::from("value\n");
        contents.push_str(&"value\n".repeat(1000));
        std::fs::write(&input, contents).unwrap();
        let connection = Connection::open_in_memory().unwrap();
        let token = Arc::new(AtomicBool::new(false));
        let cancelled = token.clone();
        let mut callbacks = 0;
        connection.progress_handler(
            100,
            Some(move || {
                callbacks += 1;
                if callbacks == 20 {
                    cancelled.store(true, Ordering::Relaxed);
                }
                false
            }),
        );
        assert!(crate::io::csv_format::insert_delimited_cancellable(
            input,
            &connection,
            b',',
            Some(&token)
        )
        .is_err());
        assert!(token.load(Ordering::Relaxed));
        assert!(connection.is_autocommit());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name='cancelled'",
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn mid_export_cancellation_removes_partial_output_and_preserves_destination() {
        let database = database(1000);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("export.csv");
        std::fs::write(&output, b"original").unwrap();
        let token = database.cancellation.clone();
        database.connection.progress_handler(
            1000,
            Some(move || {
                token.store(true, Ordering::Relaxed);
                false
            }),
        );
        assert!(database.export_delimited("t", &output, b',').is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn user_sql_cannot_commit_outside_its_rollback_boundary() {
        let database = database(3);
        assert!(database
            .sql_query("UPDATE t SET value='changed'; COMMIT;")
            .is_err());
        assert_eq!(database.get_cell("t", 1, "value").unwrap(), "v1");
        assert!(database.connection.is_autocommit());
    }

    #[test]
    fn layout_changes_share_cached_cells_without_querying_sqlite() {
        use crate::controller::Command;
        let mut controller = crate::controller::controller_impl::Controller::new(database(10));
        controller.get_headers_and_rows(100).unwrap();
        controller.view.column = 1;
        let before = controller.view.rows.clone();
        controller
            .database
            .connection
            .authorizer(Some(|_: AuthContext<'_>| Authorization::Deny));
        controller.execute(Command::HideColumn, vec![]).unwrap();
        assert!(Arc::ptr_eq(&before, &controller.view.rows));
        assert_eq!(controller.view.headers, ["id"]);
        controller.execute(Command::ShowColumns, vec![]).unwrap();
        assert!(Arc::ptr_eq(&before, &controller.view.rows));
        assert_eq!(
            controller.get_headers_and_rows(100).unwrap().1[0][1],
            DataItem::Text("v1".into())
        );
    }

    #[test]
    fn value_update_with_trigger_invalidates_counts_and_ordering() {
        let database = database(3);
        database.execute_batch("CREATE TRIGGER add_row AFTER UPDATE ON t BEGIN INSERT INTO t(value) VALUES('trigger'); END;").unwrap();
        database.get(10, 0, "t", "ORDER BY id").unwrap();
        assert_eq!(database.count_rows("t"), Some(3));
        database.update_cell("t", "value", 1, "changed").unwrap();
        assert_eq!(database.count_rows("t"), Some(4));
        assert_eq!(database.get(10, 0, "t", "ORDER BY id").unwrap().1.len(), 4);
    }

    #[test]
    fn changing_values_keeps_the_id_order_map_and_row_count_cache() {
        let database = database(10);
        database.get(10, 0, "t", "ORDER BY id").unwrap();
        database.count_rows("t").unwrap();
        database.update_cell("t", "value", 1, "changed").unwrap();
        assert!(database.cache.borrow().ordering.is_some());
        assert_eq!(database.cache.borrow().counts.get("t"), Some(&10));
        let page = database.get(10, 0, "t", "ORDER BY id").unwrap();
        assert_eq!(page.1[0][1], DataItem::Text("changed".into()));
        database.get(10, 0, "t", "ORDER BY value").unwrap();
        database.update_cell("t", "value", 1, "aaa").unwrap();
        assert!(database.cache.borrow().ordering.is_none());
        assert_eq!(
            database.get(1, 0, "t", "ORDER BY value").unwrap().1[0][0],
            DataItem::Integer(1)
        );
    }

    #[test]
    fn saves_cannot_replace_the_open_source_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.db");
        let database = Database::new(Connection::open(&path).unwrap()).unwrap();
        assert!(database.backup_db(&path).is_err());
        assert!(database
            .export_delimited("table_of_tables", &path, b',')
            .is_err());
    }

    #[test]
    fn pages_support_without_rowid_tables_and_quoted_sort_columns() {
        let database = database(0);
        database.execute_batch("CREATE TABLE w(id INTEGER PRIMARY KEY, \"a\"\"b\" TEXT) WITHOUT ROWID; INSERT INTO w VALUES (1,'z'),(2,'a');").unwrap();
        assert_eq!(
            database
                .get(1, 0, "w", "ORDER BY \"a\"\"b\" ASC")
                .unwrap()
                .1[0][0],
            DataItem::Integer(2)
        );
    }

    #[test]
    fn invalid_regex_returns_an_error_and_connection_remains_usable() {
        let database = database(2);
        assert!(database
            .connection
            .query_row("SELECT regexp('[', 'text')", [], |r| r.get::<_, bool>(0))
            .is_err());
        assert!(database
            .connection
            .query_row("SELECT regexp('text', 'text')", [], |r| r.get::<_, bool>(0))
            .unwrap());
        assert_eq!(
            database
                .connection
                .query_row(
                    "SELECT regexp_transform_with_capture_group('(a)', 'before a after', '$1$1')",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "before aa after"
        );
    }
}
