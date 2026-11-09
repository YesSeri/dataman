use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time;

use rusqlite::{backup, params, types::Value, Connection, OptionalExtension, Statement};

use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use super::datarow::DataTable;
use super::metadata::{create_catalog, populate_catalog};
use super::regexping;
use super::sql_queries;

pub(crate) const INTERNAL_PREFIX: &str = "__dataman_undo_";

use crate::model::sql_queries::quote_identifier;

mod identity;
pub use identity::{ColumnId, ColumnMetadata, Page, RowId, SheetId};

fn data_item_value(value: &DataItem) -> Value {
    match value {
        DataItem::Text(value) => Value::Text(value.clone()),
        DataItem::Integer(value) => Value::Integer(*value),
        DataItem::Float(value) => Value::Real(*value),
        DataItem::Blob(value) => Value::Blob(value.clone()),
        DataItem::Null => Value::Null,
    }
}

#[cfg(test)]
mod lazy_open_regressions {
    use super::*;

    fn source() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t(value); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO t SELECT printf('%0100d',x) FROM n;").unwrap();
        drop(conn);
        let db = Database::try_from(vec![path]).unwrap();
        (directory, db)
    }

    #[test]
    fn compatible_initial_display_has_no_count_backup_or_mutation_trackers() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let (_directory, db) = source();
        assert!(db
            .connection
            .is_readonly(rusqlite::DatabaseName::Main)
            .unwrap());
        db.connection.authorizer(Some(|context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Function {
                    function_name: "count"
                }
            ) {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }));
        let mut controller = crate::controller::controller_impl::Controller::new(db).unwrap();
        assert_eq!(controller.get_headers_and_rows(100).unwrap().1.len(), 100);
        for command in [
            crate::controller::Command::Sort,
            crate::controller::Command::Undo,
            crate::controller::Command::Redo,
        ] {
            controller.execute(command, vec![]).unwrap();
        }
        let db = &controller.database;
        assert!(db.session_directory.is_none());
        assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
        assert!(db.identity_trackers().1.is_empty());
        assert!(matches!(
            db.row_count_state("t"),
            RowCountState::Unknown { .. }
        ));
    }

    #[test]
    fn promotion_restores_catalog_handles_functions_and_key_tracking() {
        let (_directory, mut db) = source();
        db.view_metadata_table().unwrap();
        let name = db.catalog_name().to_owned();
        let catalog = db.open_sheet(&name).unwrap();
        let catalog_page = db.fetch_page(catalog, 10, 0, "").unwrap();
        let sheet = db.open_sheet("t").unwrap();
        let row = db.fetch_page(sheet, 2, 0, "").unwrap().row_ids[0];
        db.ensure_workspace().unwrap();
        let restored = db.fetch_page(catalog, 10, 0, "").unwrap();
        assert_eq!(catalog_page.rows, restored.rows);
        assert_eq!(catalog_page.row_ids, restored.row_ids);
        assert!(db
            .connection
            .query_row("SELECT 'abc' REGEXP 'a'", [], |row| row.get::<_, bool>(0))
            .unwrap());
        db.connection
            .execute_batch("UPDATE t SET rowid=-1 WHERE rowid=1")
            .unwrap();
        assert_eq!(db.fetch_page(sheet, 2, 0, "").unwrap().row_ids[0], row);
    }

    #[test]
    fn cancellation_in_each_promotion_phase_keeps_source_and_handles_usable() {
        for phase in [
            "Preparing workspace",
            "Copying database",
            "Initializing workspace",
        ] {
            let (_directory, mut db) = source();
            let sheet = db.open_sheet("t").unwrap();
            let page = db.fetch_page(sheet, 2, 0, "").unwrap();
            let token = db.cancellation.clone();
            db.progress = Some(ProgressCallback(Arc::new(move |current| {
                if current == phase {
                    token.store(true, Ordering::Relaxed);
                }
            })));
            assert!(db.ensure_workspace().is_err());
            assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
            assert!(db.session_directory.is_none());
            db.cancellation.store(false, Ordering::Relaxed);
            db.progress = None;
            assert_eq!(
                db.fetch_page(sheet, 2, 0, "").unwrap().row_ids,
                page.row_ids
            );
            db.ensure_workspace().unwrap();
            let path = db.connection.path().unwrap().to_owned();
            db.ensure_workspace().unwrap();
            assert_eq!(db.connection.path().unwrap(), path);
        }
    }

    #[test]
    fn backup_disk_full_and_initialization_failure_leave_sidecar_reusable() {
        for full in [true, false] {
            let (_directory, mut db) = source();
            let sheet = db.open_sheet("t").unwrap();
            let row = db.fetch_page(sheet, 2, 0, "").unwrap().row_ids[0];
            db.workspace_max_pages = full.then_some(1);
            db.fail_workspace_initialization = !full;
            let error = db.ensure_workspace().unwrap_err();
            if full {
                assert!(error.to_string().contains("disk is full"), "{error:?}");
            }
            assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
            assert_eq!(db.fetch_page(sheet, 2, 0, "").unwrap().row_ids[0], row);
            db.workspace_max_pages = None;
            db.fail_workspace_initialization = false;
            db.ensure_workspace().unwrap();
            assert_eq!(db.fetch_page(sheet, 2, 0, "").unwrap().row_ids[0], row);
        }
    }

    #[test]
    fn concurrent_wal_commit_discards_snapshot_results_and_source_handles() {
        let (directory, mut db) = source();
        let writer = Connection::open(directory.path().join("source.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        // Changing journal mode may itself advance the source generation.
        let _ = db.validate_source();
        let sheet = db.open_sheet("t").unwrap();
        let result = db.source_read(|| {
            let page = db.fetch_page(sheet, 2, 0, "")?;
            writer.execute_batch("UPDATE t SET value='changed' WHERE rowid=1")?;
            Ok(page)
        });
        assert!(result.unwrap_err().to_string().contains("Source changed"));
        assert!(db.fetch_page(sheet, 2, 0, "").is_err());
        assert_ne!(db.open_sheet("t").unwrap(), sheet);
    }

    #[test]
    fn rollback_journal_contention_and_cancelled_count_preserve_browsing() {
        let (directory, mut db) = source();
        let writer = Connection::open(directory.path().join("source.sqlite")).unwrap();
        db.connection
            .busy_timeout(time::Duration::from_millis(20))
            .unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(db.ensure_workspace().is_err());
        assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
        writer.execute_batch("ROLLBACK").unwrap();
        db.cancellation.store(true, Ordering::Relaxed);
        assert!(db.count_rows("t").is_err());
        assert!(matches!(
            db.row_count_state("t"),
            RowCountState::Unknown { .. }
        ));
        db.cancellation.store(false, Ordering::Relaxed);
        let sheet = db.open_sheet("t").unwrap();
        assert_eq!(db.fetch_page(sheet, 2, 0, "").unwrap().rows.len(), 2);
    }

    #[test]
    fn exact_count_invokes_progress_and_cancels_during_the_scan() {
        let (_directory, db) = source();
        let token = db.cancellation.clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        db.connection.progress_handler(
            1,
            Some(move || {
                if observed.fetch_add(1, Ordering::Relaxed) >= 500 {
                    token.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }),
        );
        assert!(db.count_rows("t").unwrap_err().is_cancelled());
        assert!(calls.load(Ordering::Relaxed) >= 500);
        assert!(matches!(
            db.row_count_state("t"),
            RowCountState::Unknown { .. }
        ));
        db.cancellation.store(false, Ordering::Relaxed);
        assert_eq!(db.count_rows("t").unwrap(), 10000);
        assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
    }
}

#[derive(Debug, Clone)]
pub(crate) enum UndoRecord {
    RowCellEdit {
        sheet: SheetId,
        column: ColumnId,
        row: RowId,
        before: DataItem,
        after: DataItem,
        is_after: bool,
    },
    RowBulkEdit {
        sheet: SheetId,
        column: ColumnId,
        archive: String,
        is_after: bool,
    },
    RowDeleted {
        sheet: SheetId,
        archive: String,
        is_deleted: bool,
    },
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

impl UndoRecord {
    fn requires_workspace(&self) -> bool {
        match self {
            Self::ViewChange => false,
            Self::Group(records) => records.iter().any(Self::requires_workspace),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinKind {
    Inner,
    Left,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregator {
    Count,
    DistinctCount,
    Sum,
    Average,
    Min,
    Max,
}

impl Aggregator {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::DistinctCount => "distinct_count",
            Self::Sum => "sum",
            Self::Average => "average",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCountState {
    Unknown { known_at_least: u64 },
    Exact(u64),
}
impl std::fmt::Display for RowCountState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exact(n) => write!(f, "{n}"),
            Self::Unknown { .. } => f.write_str("unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageState {
    ReadOnlySource,
    PrivateWorkspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceGeneration {
    data: i64,
    schema: i64,
}

pub(crate) struct ProgressCallback(pub Arc<dyn Fn(&'static str) + Send + Sync>);
impl std::fmt::Debug for ProgressCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressCallback")
    }
}

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
    // Field order matters: close SQLite before removing its directory.
    pub(crate) session_directory: Option<tempfile::TempDir>,
    identity_directory: tempfile::TempDir,
    source_path: Option<PathBuf>,
    source_identity: Option<same_file::Handle>,
    source_generation: Cell<Option<SourceGeneration>>,
    storage_state: StorageState,
    transaction_depth: Cell<u32>,
    pub(crate) source_epoch: Cell<u64>,
    pub(crate) progress: Option<ProgressCallback>,
    #[cfg(test)]
    workspace_max_pages: Option<u32>,
    #[cfg(test)]
    fail_workspace_initialization: bool,
    catalog_name: String,
    pub(crate) cancellation: Arc<AtomicBool>,
    cache: RefCell<DatabaseCache>,
    revision: Cell<u64>,
    identities: RefCell<identity::IdentityRegistry>,
}

#[derive(Debug, Default)]
struct DatabaseCache {
    headers: HashMap<String, Vec<String>>,
    counts: HashMap<String, u32>,
    lower_bounds: HashMap<String, u64>,
    ordering: Option<(String, String)>,
    names: Option<Vec<String>>,
    storage_keys: HashMap<String, String>,
    value_cache_safe: Option<bool>,
    indexed_ids: HashMap<String, bool>,
    page_cursor: Option<(String, String, u32, i64)>,
}

impl Database {
    pub(crate) fn toggle_history_record(&mut self, record: &mut UndoRecord) -> AppResult<()> {
        if record.requires_workspace() {
            self.ensure_workspace()?;
        }
        self.toggle_history_record_workspace(record)
    }
    pub(crate) fn discard_hidden_record(&mut self, record: &UndoRecord) -> AppResult<()> {
        if record.requires_workspace() {
            self.ensure_workspace()?;
        }
        self.discard_hidden_record_workspace(record)
    }
    pub fn execute_batch(&mut self, sql: &str) -> AppResult<()> {
        self.ensure_workspace()?;
        self.execute_batch_workspace(sql)
    }
    pub(crate) fn bulk_edit_selected(
        &mut self,
        table: &str,
        column: &str,
        ids: &std::collections::HashSet<i64>,
        value: &str,
    ) -> AppResult<UndoRecord> {
        self.ensure_workspace()?;
        self.bulk_edit_selected_workspace(table, column, ids, value)
    }
    pub(crate) fn delete_selected(
        &mut self,
        table: &str,
        ids: &std::collections::HashSet<i64>,
    ) -> AppResult<UndoRecord> {
        self.ensure_workspace()?;
        self.delete_selected_workspace(table, ids)
    }
    pub(crate) fn copy_selected(
        &mut self,
        source: &str,
        target: &str,
        ordering: &str,
        ids: &std::collections::HashSet<i64>,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.copy_selected_workspace(source, target, ordering, ids)
    }
    pub fn derive_column<F>(
        &mut self,
        table_name: &str,
        old_column_name: &str,
        new_column_name: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        self.ensure_workspace()?;
        self.derive_column_workspace(table_name, old_column_name, new_column_name, fun)
    }
    pub(crate) fn frequency_table(&mut self, table: &str, column: &str) -> AppResult<String> {
        self.ensure_workspace()?;
        self.frequency_table_workspace(table, column)
    }
    pub(crate) fn frequency_table_with_members(
        &mut self,
        table: &str,
        column: &str,
    ) -> AppResult<(String, String)> {
        self.ensure_workspace()?;
        self.frequency_table_with_members_workspace(table, column)
    }
    pub(crate) fn summary_table(&mut self, table: &str, column: &str) -> AppResult<String> {
        self.ensure_workspace()?;
        self.summary_table_workspace(table, column)
    }
    pub(crate) fn describe_table(&mut self, table: &str) -> AppResult<String> {
        self.ensure_workspace()?;
        self.describe_table_workspace(table)
    }
    pub(crate) fn join_tables(
        &mut self,
        left: &str,
        left_column: &str,
        right: &str,
        right_column: &str,
    ) -> AppResult<String> {
        self.ensure_workspace()?;
        self.join_tables_workspace(left, left_column, right, right_column)
    }
    pub(crate) fn join_tables_with_keys(
        &mut self,
        left: &str,
        right: &str,
        keys: &[(String, String)],
        kind: JoinKind,
    ) -> AppResult<String> {
        self.ensure_workspace()?;
        self.join_tables_with_keys_workspace(left, right, keys, kind)
    }
    pub(crate) fn append_tables(&mut self, tables: &[String]) -> AppResult<String> {
        self.ensure_workspace()?;
        self.append_tables_workspace(tables)
    }
    pub(crate) fn grouped_table(
        &mut self,
        table: &str,
        keys: &[String],
        aggregations: &[(String, Aggregator)],
    ) -> AppResult<String> {
        self.ensure_workspace()?;
        self.grouped_table_workspace(table, keys, aggregations)
    }
    pub(crate) fn pivot_table(
        &mut self,
        table: &str,
        row_keys: &[String],
        pivot_column: &str,
        aggregations: &[(String, Aggregator)],
    ) -> AppResult<String> {
        self.ensure_workspace()?;
        self.pivot_table_workspace(table, row_keys, pivot_column, aggregations)
    }
    pub fn regex_filter(
        &mut self,
        table_name: &str,
        header: &str,
        pattern: &str,
    ) -> AppResult<String> {
        self.ensure_workspace()?;
        self.regex_filter_workspace(table_name, header, pattern)
    }
    pub(crate) fn regex_capture_group_transform(
        &mut self,
        table_name: &str,
        pattern: &str,
        header: &str,
        transformation: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.regex_capture_group_transform_workspace(table_name, pattern, header, transformation)
    }
    pub(crate) fn regex_no_capture_group_transform(
        &mut self,
        table_name: &str,
        pattern: &str,
        header: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.regex_no_capture_group_transform_workspace(table_name, pattern, header)
    }
    pub(crate) fn copy(&mut self, table_name: &str, header: &str) -> AppResult<()> {
        self.ensure_workspace()?;
        self.copy_workspace(table_name, header)
    }
    pub(crate) fn sql_query(&mut self, query: &str) -> AppResult<()> {
        self.ensure_workspace()?;
        self.sql_query_workspace(query)
    }
    pub fn update_cell(
        &mut self,
        table_name: &str,
        header: &str,
        id: i64,
        content: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.update_cell_workspace(table_name, header, id, content)
    }
    pub(crate) fn text_to_int(&mut self, table_name: &str, column: &str) -> AppResult<()> {
        self.ensure_workspace()?;
        self.text_to_int_workspace(table_name, column)
    }
    pub(crate) fn int_to_text(&mut self, table_name: &str, column: &str) -> AppResult<()> {
        self.ensure_workspace()?;
        self.int_to_text_workspace(table_name, column)
    }
    pub(crate) fn delete_column(&mut self, table_name: &str, column: &str) -> AppResult<String> {
        self.ensure_workspace()?;
        self.delete_column_workspace(table_name, column)
    }
    pub(crate) fn rename_column(
        &mut self,
        table_name: &str,
        column: &str,
        new_column: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.rename_column_workspace(table_name, column, new_column)
    }
    pub(crate) fn delete_table(&mut self, table_name: &str) -> AppResult<String> {
        self.ensure_workspace()?;
        self.delete_table_workspace(table_name)
    }
    pub(crate) fn delete_tables(
        &mut self,
        table_names: &[String],
    ) -> AppResult<Vec<(String, String)>> {
        self.ensure_workspace()?;
        self.delete_tables_workspace(table_names)
    }
    pub(crate) fn rename_table(
        &mut self,
        old_table_name: &str,
        new_table_name: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.rename_table_workspace(old_table_name, new_table_name)
    }
    pub(crate) fn math_operation(
        &mut self,
        table_name: &str,
        inputs: Vec<String>,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.math_operation_workspace(table_name, inputs)
    }

    pub fn new(connection: Connection) -> AppResult<Self> {
        Self::new_cancellable(connection, Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn new_cancellable(
        connection: Connection,
        token: Arc<AtomicBool>,
    ) -> AppResult<Self> {
        if token.load(Ordering::Relaxed) {
            return Err(AppError::Cancelled);
        }
        if let Some(path) = connection.path().filter(|path| !path.is_empty()) {
            let source_path = std::fs::canonicalize(path)?;
            let source_identity = same_file::Handle::from_path(&source_path)?;
            // Even callers supplying a writable connection browse through a read-only handle.
            let path = source_path
                .to_str()
                .ok_or_else(|| AppError::from("Source path is not valid Unicode"))?;
            let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                format!("//{}", unc.replace('\\', "/"))
            } else {
                path.strip_prefix(r"\\?\")
                    .unwrap_or(path)
                    .replace('\\', "/")
            };
            let mut uri = String::from("file:");
            // Keep UNC hosts in the URI path, rather than the authority field.
            if path.starts_with("//") {
                uri.push_str("//");
            }
            for byte in path.bytes() {
                if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
                    uri.push(char::from(byte));
                } else {
                    uri.push_str(&format!("%{byte:02X}"));
                }
            }
            uri.push_str("?mode=ro");
            // SQLite retains the supplied open flags for ATTACH, while the URI
            // restricts main itself to read-only. Session storage remains writable.
            let source = Connection::open_with_flags(
                uri,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )?;
            // Busy waits do not run SQLite's progress handler. Bound them so
            // lock contention cannot delay cancellation for several seconds.
            source.busy_timeout(time::Duration::from_millis(50))?;
            let mut database = Self::from_workspace_cancellable(source, token.clone())?;
            database.source_path = Some(source_path);
            database.source_identity = Some(source_identity);
            database.storage_state = StorageState::ReadOnlySource;
            database.source_generation.set(Some(database.generation()?));
            database.cancellation = token.clone();
            Self::install_cancellation(&database.connection, token);
            return Ok(database);
        }
        let mut database = Self::from_workspace_cancellable(connection, token.clone())?;
        database.cancellation = token.clone();
        Self::install_cancellation(&database.connection, token);
        Ok(database)
    }

    pub(crate) fn from_workspace(connection: Connection) -> AppResult<Self> {
        Self::from_workspace_cancellable(connection, Arc::new(AtomicBool::new(false)))
    }

    fn from_workspace_cancellable(
        connection: Connection,
        cancellation: Arc<AtomicBool>,
    ) -> AppResult<Self> {
        Self::install_cancellation(&connection, cancellation.clone());
        connection.pragma_update(None, "foreign_keys", true)?;
        let identity_directory = tempfile::Builder::new()
            .prefix("dataman-identities-")
            .tempdir()?;
        // On initialization errors, close attached files before TempDir cleanup
        // as well as on the successful Database field-drop path.
        let connection = connection;
        // ATTACH on a read-only main cannot create a missing session file.
        drop(Connection::open(
            identity_directory.path().join("identity.sqlite"),
        )?);
        super::metadata::initialize_ownership_at(
            &connection,
            &identity_directory.path().join("identity.sqlite"),
        )?;
        let catalog_name = create_catalog(&connection)?;
        if let Err(err) = regexping::custom_functions::add_custom_functions(&connection) {
            log::info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            Self::install_cancellation(&connection, cancellation.clone());
            Ok(Database {
                connection,
                session_directory: None,
                identity_directory,
                source_path: None,
                source_identity: None,
                source_generation: Cell::new(None),
                storage_state: StorageState::PrivateWorkspace,
                transaction_depth: Cell::new(0),
                source_epoch: Cell::new(0),
                progress: None,
                #[cfg(test)]
                workspace_max_pages: None,
                #[cfg(test)]
                fail_workspace_initialization: false,
                catalog_name,
                cancellation,
                cache: RefCell::new(DatabaseCache::default()),
                revision: Cell::new(0),
                identities: RefCell::new(identity::IdentityRegistry::new()),
            })
        }
    }
    pub fn storage_state(&self) -> StorageState {
        self.storage_state
    }

    fn generation(&self) -> AppResult<SourceGeneration> {
        let path = self
            .source_path
            .as_ref()
            .ok_or_else(|| AppError::from("Missing source path"))?;
        let current = same_file::Handle::from_path(path)
            .map_err(|_| AppError::from("Source is inaccessible; reopen the file"))?;
        if self.source_identity.as_ref() != Some(&current) {
            return Err(AppError::from("Source path was replaced; reopen the file"));
        }
        Ok(SourceGeneration {
            data: self
                .connection
                .pragma_query_value(None, "data_version", |r| r.get(0))?,
            schema: self
                .connection
                .pragma_query_value(None, "schema_version", |r| r.get(0))?,
        })
    }

    pub(crate) fn validate_source(&self) -> AppResult<()> {
        if self.storage_state != StorageState::ReadOnlySource {
            return Ok(());
        }
        let generation = self.generation()?;
        if self.source_generation.get() != Some(generation) {
            self.source_generation.set(Some(generation));
            self.invalidate();
            self.identities.borrow_mut().reset_source();
            self.source_epoch
                .set(self.source_epoch.get().wrapping_add(1));
            return Err(AppError::from("Source changed; review and retry"));
        }
        Ok(())
    }

    fn source_read<T>(&self, operation: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
        if self.storage_state != StorageState::ReadOnlySource || self.transaction_depth.get() > 0 {
            return operation();
        }
        self.validate_source()?;
        let result = self.atomic_impl(operation, false);
        // Results and any newly interned handles must not escape a changed snapshot.
        self.validate_source()?;
        result
    }

    pub(crate) fn ensure_workspace(&mut self) -> AppResult<()> {
        if self.storage_state == StorageState::PrivateWorkspace {
            return Ok(());
        }
        self.report_phase("Preparing workspace");
        self.check_cancelled()?;
        self.validate_source()?;
        if self.transaction_depth.get() != 0 || !self.connection.is_autocommit() {
            return Err(AppError::from(
                "Workspace promotion must precede the operation transaction",
            ));
        }
        let directory = tempfile::Builder::new().prefix("dataman-").tempdir()?;
        let mut workspace = Connection::open(directory.path().join("session.sqlite"))?;
        #[cfg(test)]
        if let Some(pages) = self.workspace_max_pages {
            workspace.pragma_update(None, "max_page_count", pages)?;
        }
        self.report_phase("Copying database");
        self.source_read(|| {
            // Establish the main snapshot before starting the bounded backup.
            self.connection
                .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
            let backup = backup::Backup::new(&self.connection, &mut workspace)?;
            let started = time::Instant::now();
            loop {
                self.check_cancelled()?;
                match backup.step(128).map_err(|error| {
                    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                        AppError::from("Copying database failed: temporary disk is full")
                    } else {
                        AppError::from(error)
                    }
                })? {
                    backup::StepResult::Done => break,
                    backup::StepResult::More => {}
                    _ if started.elapsed() < time::Duration::from_secs(5) => {
                        std::thread::sleep(time::Duration::from_millis(10))
                    }
                    _ => {
                        return Err(AppError::from(
                            "Timed out waiting for a consistent SQLite snapshot",
                        ))
                    }
                }
            }
            Ok(())
        })?;
        self.report_phase("Initializing workspace");
        let mut candidate = Self::from_workspace_cancellable(workspace, self.cancellation.clone())?;
        candidate
            .connection
            .execute_batch("DETACH DATABASE __dataman_session")?;
        let sidecar = self.identity_directory.path().join("identity.sqlite");
        candidate.connection.execute(
            "ATTACH DATABASE ?1 AS __dataman_session",
            [sidecar
                .to_str()
                .ok_or_else(|| AppError::from("Invalid session path"))?],
        )?;
        candidate.connection.execute_batch(
            "PRAGMA __dataman_session.cache_size=-2048; PRAGMA __dataman_session.synchronous=OFF;",
        )?;
        candidate.cancellation = self.cancellation.clone();
        Self::install_cancellation(&candidate.connection, self.cancellation.clone());
        candidate.catalog_name = self.catalog_name.clone();
        create_catalog(&candidate.connection)?;
        *candidate.identities.borrow_mut() = self.identities.borrow().clone();
        populate_catalog(&candidate.connection, &candidate.catalog_name)?;
        candidate.reinstall_identity_tracking()?;
        #[cfg(test)]
        if self.fail_workspace_initialization {
            return Err(AppError::from("Injected workspace initialization failure"));
        }
        self.validate_source()?;
        self.check_cancelled()?;
        Self::install_cancellation(&candidate.connection, self.cancellation.clone());
        // Publication is the only replacement of the live connection. The source
        // and its attached sidecar remain usable until every candidate step succeeds.
        *self.identities.borrow_mut() = candidate.identities.into_inner();
        self.connection = candidate.connection;
        self.session_directory = Some(directory);
        self.storage_state = StorageState::PrivateWorkspace;
        self.cache.borrow_mut().page_cursor = None;
        self.cache.borrow_mut().ordering = None;
        self.identities.borrow_mut().invalidate_orders();
        self.report_phase("Working");
        Ok(())
    }

    fn report_phase(&self, phase: &'static str) {
        if let Some(callback) = &self.progress {
            (callback.0)(phase);
        }
    }

    pub(crate) fn install_cancellation(connection: &Connection, token: Arc<AtomicBool>) {
        connection.progress_handler(1000, Some(move || token.load(Ordering::Relaxed)));
    }

    pub(crate) fn catalog_name(&self) -> &str {
        &self.catalog_name
    }

    pub(crate) fn check_cancelled(&self) -> AppResult<()> {
        if self.cancellation.load(Ordering::Relaxed) {
            Err(AppError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.get()
    }

    pub(crate) fn invalidate(&self) {
        self.identities.borrow_mut().invalidate_orders();
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
            let safe = self.connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger') AND NOT EXISTS(SELECT 1 FROM sqlite_master m, pragma_foreign_key_list(m.name) f WHERE m.type='table')", [], |row| row.get::<_,bool>(0))?;
            self.cache.borrow_mut().value_cache_safe = Some(safe);
            safe
        };
        if !safe {
            return self.atomic(operation);
        }
        let result = self.atomic_impl(operation, false);
        if result.is_ok() {
            self.identities.borrow_mut().invalidate_orders();
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
        if self.transaction_depth.get() > 0 {
            return operation();
        }
        if !self.connection.is_autocommit() {
            return Err(AppError::from(
                "Unexpected transaction outside an operation",
            ));
        }
        struct Depth<'a>(&'a Cell<u32>);
        impl Drop for Depth<'_> {
            fn drop(&mut self) {
                self.0.set(0);
            }
        }
        self.transaction_depth.set(1);
        let _depth = Depth(&self.transaction_depth);
        let identities_before = self.identities.borrow().clone();
        // Keep the transaction guard for rollback during unwinding. Commit and
        // rollback are explicit so failures from both can be retained.
        let mut transaction = self.connection.unchecked_transaction()?;
        let result = operation().and_then(|value| {
            self.check_cancelled()?;
            Ok(value)
        });
        // COMMIT/ROLLBACK are the operation boundary. A late cancellation must not
        // interrupt rollback or turn a successfully committed mutation into an error.
        self.connection.progress_handler(0, None::<fn() -> bool>);
        let result = result.and_then(|value| {
            self.connection.execute_batch("COMMIT")?;
            Ok(value)
        });
        let result = match result {
            Err(error) => {
                let cleanup = if self.connection.is_autocommit() {
                    Ok(())
                } else {
                    self.connection
                        .execute_batch("ROLLBACK")
                        .map_err(AppError::from)
                };
                crate::error::finish(
                    Err(error.normalize_cancelled(self.cancellation.load(Ordering::Relaxed))),
                    cleanup,
                )
            }
            success => success,
        };
        transaction.set_drop_behavior(rusqlite::DropBehavior::Ignore);
        Self::install_cancellation(&self.connection, self.cancellation.clone());
        if result.is_err() {
            self.restore_registry(identities_before);
        }
        if invalidate_all || result.is_err() {
            self.invalidate();
        }
        result
    }

    fn validate_destination(&self, destination: &Path) -> AppResult<()> {
        if let Some(source) = &self.source_path {
            if std::fs::canonicalize(destination).is_ok_and(|path| path == *source)
                || same_file::Handle::from_path(destination)
                    .ok()
                    .as_ref()
                    .is_some_and(|handle| Some(handle) == self.source_identity.as_ref())
            {
                return Err(AppError::from(
                    "Cannot replace a source database; save to a different path",
                ));
            }
        }
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
        self.source_read(|| self.export_delimited_in_snapshot(table, destination, delimiter))
    }

    fn export_delimited_in_snapshot<P: AsRef<Path>>(
        &self,
        table: &str,
        destination: P,
        delimiter: u8,
    ) -> AppResult<()> {
        let context = format!(
            "Exporting sheet '{}' to '{}'",
            table,
            destination.as_ref().display()
        );
        (|| {
            self.validate_destination(destination.as_ref())?;
            crate::io::csv_format::save_delimited_cancellable(
                &self.connection,
                table,
                &destination.as_ref().to_path_buf(),
                delimiter,
                Some(&self.cancellation),
            )
        })()
        .map_err(|error: AppError| {
            error
                .normalize_cancelled(self.cancellation.load(Ordering::Relaxed))
                .context(context)
        })
    }

    pub(crate) fn backup_db<P: AsRef<Path>>(&self, destination: P) -> AppResult<()> {
        self.source_read(|| self.backup_db_in_snapshot(destination))
    }

    fn backup_db_in_snapshot<P: AsRef<Path>>(&self, destination: P) -> AppResult<()> {
        let destination = destination.as_ref();
        let context = format!("Saving database to '{}'", destination.display());
        (|| {
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
                    match backup.step(128).map_err(|error| {
                        if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                            AppError::from("Copying database failed: temporary disk is full")
                        } else {
                            AppError::from(error)
                        }
                    })? {
                        backup::StepResult::Done => break,
                        backup::StepResult::More => {}
                        _ => std::thread::sleep(time::Duration::from_millis(10)),
                    }
                }
            }
            {
                let target = Connection::open(temporary.path())?;
                Self::install_cancellation(&target, self.cancellation.clone());
                self.remove_owned_artifacts_from(&target)?;
            }
            self.check_cancelled()?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(destination)
                .map_err(|e| AppError::Io(e.error))?;
            Ok(())
        })()
        .map_err(|error: AppError| {
            error
                .normalize_cancelled(self.cancellation.load(Ordering::Relaxed))
                .context(context)
        })
    }

    fn storage_key(&self, table: &str) -> AppResult<String> {
        if let Some(key) = self.cache.borrow().storage_keys.get(table) {
            return Ok(key.clone());
        }
        let headers = self.get_headers(table)?;
        for candidate in ["rowid", "_rowid_", "oid"] {
            if headers
                .iter()
                .any(|name| name.eq_ignore_ascii_case(candidate))
            {
                continue;
            }
            match self.connection.prepare(&format!(
                "SELECT {candidate} FROM {} LIMIT 0",
                quote_identifier(table)
            )) {
                Ok(_) => {
                    self.cache
                        .borrow_mut()
                        .storage_keys
                        .insert(table.to_owned(), candidate.to_owned());
                    return Ok(candidate.to_owned());
                }
                Err(rusqlite::Error::SqliteFailure(code, Some(message)))
                    if code.code == rusqlite::ErrorCode::Unknown
                        && message == format!("no such column: {candidate}") => {}
                Err(rusqlite::Error::SqlInputError { error, msg, .. })
                    if error.code == rusqlite::ErrorCode::Unknown
                        && msg == format!("no such column: {candidate}") => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.connection.prepare(&format!(
            "SELECT id FROM {} LIMIT 0",
            quote_identifier(table)
        ))?;
        Ok("id".to_owned())
    }

    /// Build an indexed, deterministic position map once per table/order revision.
    fn ensure_ordering(&self, table: &str, ordering: &str) -> AppResult<()> {
        let key = (table.to_owned(), ordering.to_owned());
        if self.cache.borrow().ordering.as_ref() == Some(&key) {
            return Ok(());
        }
        let identity = if self.indexed_id_order(table, "")?.is_some() {
            "id"
        } else {
            // A user column named id may be text or contain duplicates.
            // Positional browsing must use the unique storage locator instead.
            ""
        };
        let storage_key = self.storage_key(table)?;
        let identity = if identity.is_empty() {
            storage_key.as_str()
        } else {
            identity
        };
        let order = if ordering.is_empty() {
            format!("ORDER BY {}", quote_identifier(identity))
        } else {
            format!("{ordering}, {} ASC", quote_identifier(identity))
        };
        self.cache.borrow_mut().ordering = None;
        self.connection.execute_batch("DROP TABLE IF EXISTS temp.__dataman_undo_page_order;
            CREATE TEMP TABLE __dataman_undo_page_order (position INTEGER PRIMARY KEY, row_id INTEGER UNIQUE, storage_key INTEGER);")?;
        self.connection.execute(&format!(
            "INSERT INTO temp.__dataman_undo_page_order SELECT ROW_NUMBER() OVER ({order}) - 1, {}, {} FROM {};",
            quote_identifier(identity), quote_identifier(&storage_key), quote_identifier(table)), [])?;
        self.cache.borrow_mut().ordering = Some(key);
        Ok(())
    }

    /// Value-only compatibility paging. Prefer `open_sheet` and `fetch_page`
    /// for generic identities and row handles that survive editing and sorting.
    pub fn get(
        &mut self,
        limit: u32,
        offset: u32,
        table_name: &str,
        ordering: &str,
    ) -> AppResult<DataTable> {
        let name = if self.storage_state == StorageState::ReadOnlySource {
            let sheet = self.open_sheet(table_name)?;
            self.sheet_name(sheet)?
        } else {
            table_name.to_owned()
        };
        self.source_read(|| self.get_in_snapshot(limit, offset, &name, ordering))
    }

    fn get_in_snapshot(
        &self,
        limit: u32,
        offset: u32,
        table_name: &str,
        ordering: &str,
    ) -> AppResult<DataTable> {
        self.check_cancelled()?;
        let headers = self.get_headers(table_name)?;
        if ordering.is_empty() && !headers.iter().any(|header| header == "id") {
            let page = self.fetch_page(
                self.open_sheet_existing(table_name)?,
                limit,
                offset,
                ordering,
            )?;
            return Ok((
                page.columns.into_iter().map(|column| column.name).collect(),
                page.rows,
            ));
        }

        let columns = headers
            .iter()
            .map(|h| format!("t.{}", quote_identifier(h)))
            .collect::<Vec<_>>()
            .join(", ");
        let direct = self.indexed_id_order(table_name, ordering)?;
        let query = if let Some(descending) = direct {
            let cursor = self.cache.borrow().page_cursor.clone();
            let direction = if descending { "DESC" } else { "ASC" };
            let resume = cursor.filter(|(table, order, next, _)| {
                table == table_name && order == ordering && *next == offset
            });
            let clause = if let Some((_, _, _, last)) = resume {
                format!(
                    "WHERE t.id {} {last} ORDER BY t.id {direction} LIMIT {limit}",
                    if descending { "<" } else { ">" }
                )
            } else {
                format!("ORDER BY t.id {direction} LIMIT {limit} OFFSET {offset}")
            };
            format!(
                "SELECT {columns} FROM {} t {clause}",
                quote_identifier(table_name)
            )
        } else {
            self.ensure_ordering(table_name, ordering)?;
            let storage_key = self.storage_key(table_name)?;
            format!(
            "SELECT {columns} FROM temp.__dataman_undo_page_order p JOIN {} t ON t.{} = p.storage_key \
             WHERE p.position >= {} AND p.position < {} ORDER BY p.position;",
            quote_identifier(table_name),
            quote_identifier(&storage_key),
            offset,
            u64::from(offset) + u64::from(limit)
            )
        };

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
        if direct.is_some() {
            if let Some(DataItem::Integer(last)) = headers
                .iter()
                .position(|h| h == "id")
                .and_then(|index| data_rows.last()?.get(index))
            {
                self.cache.borrow_mut().page_cursor = Some((
                    table_name.to_owned(),
                    ordering.to_owned(),
                    offset.saturating_add(data_rows.len() as u32),
                    *last,
                ));
            }
        }
        Ok((headers, data_rows))
    }

    /// Only a single INTEGER PRIMARY KEY guarantees the index and unique tie-breaker
    /// required here. Other orders retain the cancellable positional-map path.
    fn indexed_id_order(&self, table: &str, ordering: &str) -> AppResult<Option<bool>> {
        let descending = match ordering {
            "" | "ORDER BY id" | "ORDER BY id ASC" | "ORDER BY \"id\" ASC" => false,
            "ORDER BY id DESC" | "ORDER BY \"id\" DESC" => true,
            _ => return Ok(None),
        };
        let cached = self.cache.borrow().indexed_ids.get(table).copied();
        let indexed = if let Some(indexed) = cached {
            indexed
        } else {
            let mut statement = self
                .connection
                .prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))?;
            let columns = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let keys = columns
                .iter()
                .filter(|(_, _, pk, _)| *pk > 0)
                .collect::<Vec<_>>();
            let mut indexed =
                keys.len() == 1 && keys[0].0 == "id" && keys[0].1.eq_ignore_ascii_case("INTEGER");
            // INTEGER PRIMARY KEY DESC can use an ordinary nullable index
            // rather than aliasing rowid. Null keys cannot drive this cursor.
            if indexed && !keys[0].3 {
                let separate_index = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1) WHERE origin='pk')",
                    [table],
                    |row| row.get::<_, bool>(0),
                )?;
                indexed = !separate_index;
            }
            self.cache
                .borrow_mut()
                .indexed_ids
                .insert(table.to_owned(), indexed);
            indexed
        };
        Ok(indexed.then_some(descending))
    }
    pub fn row_count_state(&self, table: &str) -> RowCountState {
        let cache = self.cache.borrow();
        cache
            .counts
            .get(table)
            .map(|n| RowCountState::Exact(u64::from(*n)))
            .unwrap_or(RowCountState::Unknown {
                known_at_least: cache.lower_bounds.get(table).copied().unwrap_or(0),
            })
    }

    pub fn count_rows(&self, table_name: &str) -> AppResult<u32> {
        self.source_read(|| self.count_rows_in_snapshot(table_name))
    }

    fn count_rows_in_snapshot(&self, table_name: &str) -> AppResult<u32> {
        (|| {
            self.check_cancelled()?;
            if let Some(count) = self.cache.borrow().counts.get(table_name) {
                return Ok(*count);
            }
            // A WHERE clause avoids SQLite's single OP_Count btree traversal,
            // which checks sqlite3_interrupt but does not invoke progress hooks.
            // The ordinary aggregate scan gives the worker bounded cancellation.
            let count = self.connection.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE 1;",
                    quote_identifier(table_name)
                ),
                [],
                |row| row.get::<_, i64>(0),
            )?;
            let count = u32::try_from(count)
                .map_err(|_| AppError::from("Row count exceeds supported u32 range"))?;
            self.cache
                .borrow_mut()
                .counts
                .insert(table_name.to_owned(), count);
            Ok(count)
        })()
        .map_err(|error: AppError| {
            error
                .normalize_cancelled(self.cancellation.load(Ordering::Relaxed))
                .context(format!("Counting rows in sheet '{table_name}'"))
        })
    }
    /// Integer-key compatibility helper. New callers should use `cell_value`.
    pub fn get_cell(&self, table_name: &str, id: i64, header: &str) -> AppResult<String> {
        Ok(self.get_cell_item(table_name, id, header)?.to_string())
    }

    pub(crate) fn get_cell_item(
        &self,
        table_name: &str,
        id: i64,
        header: &str,
    ) -> AppResult<DataItem> {
        self.source_read(|| self.get_cell_item_in_snapshot(table_name, id, header))
    }

    fn get_cell_item_in_snapshot(
        &self,
        table_name: &str,
        id: i64,
        header: &str,
    ) -> AppResult<DataItem> {
        let key = self.legacy_integer_key(table_name)?;
        let query = format!(
            "SELECT {} FROM {} WHERE {} = ?;",
            quote_identifier(header),
            quote_identifier(table_name),
            quote_identifier(&key)
        );
        let mut stmt = self.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        let row = rows
            .next()?
            .ok_or_else(|| AppError::from("Cell row not found"))?;
        Ok(DataItem::from(row.get_ref(0)?))
    }

    pub(crate) fn toggle_history_record_workspace(&self, record: &mut UndoRecord) -> AppResult<()> {
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
                UndoRecord::RowCellEdit {
                    sheet,
                    column,
                    row,
                    before,
                    after,
                    is_after,
                } => {
                    self.edit_row_value(
                        *sheet,
                        *row,
                        *column,
                        if *is_after { before } else { after },
                    )?;
                    *is_after = !*is_after;
                }
                UndoRecord::RowBulkEdit {
                    sheet,
                    column,
                    archive,
                    is_after,
                } => {
                    self.restore_row_bulk(*sheet, *column, archive, *is_after)?;
                    *is_after = !*is_after;
                }
                UndoRecord::RowDeleted {
                    sheet,
                    archive,
                    is_deleted,
                } => {
                    self.restore_row_deletion(*sheet, archive, *is_deleted)?;
                    *is_deleted = !*is_deleted;
                }
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
                    self.rename_table_workspace(from, to)?;
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
                    self.rename_column_workspace(table, from, to)?;
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
                        } else {
                            *hidden_name = self.internal_table_name()?;
                        }
                        (visible_name, hidden_name)
                    } else {
                        (hidden_name, visible_name)
                    };
                    match table {
                        Some(table) => self.rename_column_workspace(table, from, to)?,
                        None => self.rename_table_workspace(from, to)?,
                    }
                    *is_visible = !*is_visible;
                }
                UndoRecord::Group(records) => {
                    for record in records {
                        self.toggle_history_record_workspace(record)?;
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
                super::metadata::own(&self.connection, &format!("column:{_table}"), &candidate)?;
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
                super::metadata::own(&self.connection, "table", &candidate)?;
                return Ok(candidate);
            }
            suffix += 1;
        }
    }

    pub(crate) fn discard_hidden_record_workspace(&self, record: &UndoRecord) -> AppResult<()> {
        if let UndoRecord::RowDeleted { sheet, archive, .. } = record {
            self.discard_deleted_handles(*sheet, archive)?;
        }
        if let UndoRecord::Group(records) = record {
            for record in records {
                self.discard_hidden_record_workspace(record)?;
            }
            return Ok(());
        }
        if let UndoRecord::RowBulkEdit { archive, .. }
        | UndoRecord::RowDeleted { archive, .. }
        | UndoRecord::StoredBulkEdit { archive, .. }
        | UndoRecord::StoredDeletedRows { archive, .. } = record
        {
            self.connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS {};",
                quote_identifier(archive)
            ))?;
            self.forget_sheet_identity(archive)?;
            super::metadata::disown(&self.connection, "table", archive)?;
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
                            self.execute_workspace_batch(&format!(
                                "ALTER TABLE {} DROP COLUMN {};",
                                quote_identifier(&table),
                                quote_identifier(hidden_name)
                            ))?;
                            self.forget_column_identity(&table, hidden_name);
                            super::metadata::disown(
                                &self.connection,
                                &format!("column:{table}"),
                                hidden_name,
                            )?;
                            break;
                        }
                    }
                }
                None => {
                    self.execute_workspace_batch(&format!(
                        "DROP TABLE IF EXISTS {};",
                        quote_identifier(hidden_name)
                    ))?;
                    self.forget_sheet_identity(hidden_name)?;
                    super::metadata::disown(&self.connection, "table", hidden_name)?;
                }
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

    fn remove_owned_artifacts_from(&self, conn: &Connection) -> AppResult<()> {
        {
            let mut statement = self
                .connection
                .prepare("SELECT name FROM __dataman_session.owned_objects WHERE scope='index'")?;
            for name in statement.query_map([], |row| row.get::<_, String>(0))? {
                conn.execute(
                    &format!("DROP INDEX IF EXISTS {}", quote_identifier(&name?)),
                    [],
                )?;
            }
        }
        let tables = {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
            let values = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            values
        };
        for table in tables {
            if super::metadata::is_owned(&self.connection, "table", &table)? {
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
            for column in columns {
                if super::metadata::is_owned(&self.connection, &format!("column:{table}"), &column)?
                    || super::metadata::is_owned(
                        &self.connection,
                        &format!("export:{table}"),
                        &column,
                    )?
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

    /// Execute an explicit workspace-only SQL batch transactionally. Successful
    /// mutations invalidate affected row handles; schema changes reset all handles.
    pub(crate) fn execute_batch_workspace(&self, sql: &str) -> AppResult<()> {
        self.sql_query_workspace(sql)
    }

    fn execute_workspace_batch(&self, sql: &str) -> AppResult<()> {
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

    pub(crate) fn bulk_edit_selected_workspace(
        &self,
        table: &str,
        column: &str,
        ids: &std::collections::HashSet<i64>,
        value: &str,
    ) -> AppResult<UndoRecord> {
        self.require_integer_identity(table)?;
        let archive = self.atomic_values(table, column, || {
            let archive = self.internal_table_name()?;
            self.load_selection(ids)?;
            self.connection.execute(&format!("CREATE TABLE {} AS SELECT t.id, t.{} AS before_value, ? AS after_value FROM {} t JOIN temp.__dataman_undo_selected s ON s.id=t.id",
                quote_identifier(&archive), quote_identifier(column), quote_identifier(table)), [value])?;
            self.connection.execute_batch(&format!("CREATE UNIQUE INDEX {} ON {} (id);",
                quote_identifier(&format!("{archive}_id")), quote_identifier(&archive)))?;
            self.connection.execute(&format!("UPDATE {} SET {} = ? WHERE id IN (SELECT id FROM temp.__dataman_undo_selected)",
                quote_identifier(table), quote_identifier(column)), [value])?;
            Ok(archive)
        })?;
        Ok(UndoRecord::StoredBulkEdit {
            table: table.into(),
            column: column.into(),
            archive,
            is_after: true,
        })
    }

    pub(crate) fn delete_selected_workspace(
        &self,
        table: &str,
        ids: &std::collections::HashSet<i64>,
    ) -> AppResult<UndoRecord> {
        self.require_integer_identity(table)?;
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

    pub(crate) fn copy_selected_workspace(
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
        self.source_read(|| self.select_regex_in_snapshot(table, column, pattern))
    }

    fn select_regex_in_snapshot(
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

    pub(crate) fn derive_column_workspace<F>(
        &self,
        table_name: &str,
        old_column_name: &str,
        new_column_name: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        self.derive_by_locator(table_name, old_column_name, new_column_name, fun)
    }

    pub fn get_table_names(&self) -> AppResult<Vec<String>> {
        if let Some(names) = &self.cache.borrow().names {
            return Ok(names.clone());
        }
        let query =
            r#"SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY rowid;"#;
        let mut stmt = self.prepare(query)?;
        let mut rows = stmt.query([])?;
        let mut table_names = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if !super::metadata::is_owned(&self.connection, "table", &name)?
                && !name.starts_with("sqlite_")
            {
                table_names.push(name);
            }
        }
        table_names.push(self.catalog_name.clone());
        self.cache.borrow_mut().names = Some(table_names.clone());
        Ok(table_names)
    }
    fn find_unused_table_name(&self, table_name: &str) -> AppResult<String> {
        let mut new_table_name = table_name.to_string();
        let mut suffix = 1;
        while self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 COLLATE NOCASE UNION ALL SELECT 1 FROM sqlite_temp_master WHERE name=?1 COLLATE NOCASE)", [&new_table_name], |row| row.get::<_, bool>(0))? {
            new_table_name = format!("{}_{}", table_name, suffix);
            suffix += 1;
        }
        Ok(new_table_name)
    }

    pub(crate) fn frequency_table_workspace(&self, table: &str, column: &str) -> AppResult<String> {
        let (result, membership) = self.frequency_table_with_members_workspace(table, column)?;
        self.execute_workspace_batch(&format!("DROP TABLE {};", quote_identifier(&membership)))?;
        Ok(result)
    }

    pub(crate) fn frequency_table_with_members_workspace(
        &self,
        table: &str,
        column: &str,
    ) -> AppResult<(String, String)> {
        let frequency = self.find_unused_table_name(&format!("{table}_frequency"))?;
        let members = self.internal_table_name()?;
        let source_table = table.replace('\'', "''");
        let source_column = column.replace('\'', "''");
        let (locator_definitions, locator_projection, first_locator) =
            self.frequency_locator_sql(table)?;
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} AS value, COUNT(*) AS count, 100.0 * COUNT(*) / NULLIF((SELECT COUNT(*) FROM {}), 0) AS percentage, '{}' AS source_table, '{}' AS source_column FROM {} GROUP BY {} ORDER BY count DESC; CREATE TABLE {} (group_id INTEGER NOT NULL, source_id, {locator_definitions}); INSERT INTO {} SELECT frequency.id, {first_locator}, {locator_projection} FROM {} AS source JOIN {} AS frequency ON source.{} IS frequency.value;",
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
        self.mark_synthetic_column(&frequency, "id")?;
        self.register_frequency(&members, table, &frequency)?;
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

    pub(crate) fn summary_table_workspace(&self, table: &str, column: &str) -> AppResult<String> {
        let result = self.find_unused_table_name(&format!("{table}_summary"))?;
        let query = format!(
            "CREATE TABLE {} AS SELECT 1 AS id, COUNT(*) AS count, COUNT(*) - COUNT({}) AS null_count, MIN({}) AS min, MAX({}) AS max, AVG(CAST({} AS REAL)) AS average FROM {};",
            quote_identifier(&result), quote_identifier(column), quote_identifier(column), quote_identifier(column), quote_identifier(column), quote_identifier(table)
        );
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, "id")?;
        Ok(result)
    }

    pub(crate) fn describe_table_workspace(&self, table: &str) -> AppResult<String> {
        let result = self.find_unused_table_name(&format!("{table}_describe"))?;
        let columns = self.data_columns(table)?;
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
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, "id")?;
        Ok(result)
    }

    pub(crate) fn join_tables_workspace(
        &self,
        left: &str,
        left_column: &str,
        right: &str,
        right_column: &str,
    ) -> AppResult<String> {
        self.join_tables_with_keys_workspace(
            left,
            right,
            &[(left_column.into(), right_column.into())],
            JoinKind::Inner,
        )
    }

    pub(crate) fn join_tables_with_keys_workspace(
        &self,
        left: &str,
        right: &str,
        keys: &[(String, String)],
        kind: JoinKind,
    ) -> AppResult<String> {
        if keys.is_empty() {
            return Err(AppError::from("At least one join key is required"));
        }
        if left == right {
            return Err(AppError::from("Choose two different tables"));
        }
        let left_headers = self.get_headers(left)?;
        let right_headers = self.get_headers(right)?;
        let mut conditions = Vec::with_capacity(keys.len());
        for (left_key, right_key) in keys {
            if !left_headers.contains(left_key) || !right_headers.contains(right_key) {
                return Err(AppError::from("Join column does not exist"));
            }
            let left_type = self.column_declared_type(left, left_key)?;
            let right_type = self.column_declared_type(right, right_key)?;
            if Self::type_affinity(&left_type) != Self::type_affinity(&right_type) {
                return Err(AppError::invalid_input(
                    "Incompatible join key types: {left_type} and {right_type}",
                ));
            }
            conditions.push(format!(
                "{}.{} = {}.{}",
                quote_identifier(left),
                quote_identifier(left_key),
                quote_identifier(right),
                quote_identifier(right_key)
            ));
        }
        let result = self.find_unused_table_name(&format!("{left}_{right}_join"))?;
        let mut columns = Vec::new();
        let mut aliases = HashSet::new();
        for (source, headers) in [(left, left_headers), (right, right_headers)] {
            for (header, _) in self.data_columns(source)? {
                let alias = Self::unique_output_name(format!("{source}_{header}"), &mut aliases);
                columns.push(format!(
                    "{}.{} AS {}",
                    quote_identifier(source),
                    quote_identifier(&header),
                    quote_identifier(&alias)
                ));
            }
        }
        if columns.is_empty() {
            columns.push("1 AS value".into());
        }
        let join_type = match kind {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Full => "FULL OUTER JOIN",
        };
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} FROM {} {} {} ON {};",
            quote_identifier(&result),
            columns.join(", "),
            quote_identifier(left),
            join_type,
            quote_identifier(right),
            conditions.join(" AND ")
        );
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, "id")?;
        Ok(result)
    }

    pub(crate) fn append_tables_workspace(&self, tables: &[String]) -> AppResult<String> {
        if tables.len() < 2 {
            return Err(AppError::from("Append requires at least two tables"));
        }
        let mut headers = Vec::new();
        let mut known = HashSet::new();
        for table in tables {
            for (header, _) in self.data_columns(table)? {
                if known.insert(header.clone()) {
                    headers.push(header);
                }
            }
        }
        if headers.is_empty() {
            return Err(AppError::from("Tables have no data columns to append"));
        }
        let result = self.find_unused_table_name("append")?;
        let synthetic = Self::synthetic_name(&headers);
        let selects = tables
            .iter()
            .map(|table| {
                let table_headers = self.get_headers(table)?;
                let values = headers
                    .iter()
                    .map(|header| {
                        if table_headers.contains(header) {
                            format!(
                                "{} AS {}",
                                quote_identifier(header),
                                quote_identifier(header)
                            )
                        } else {
                            format!("NULL AS {}", quote_identifier(header))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok::<_, AppError>(format!("SELECT {values} FROM {}", quote_identifier(table)))
            })
            .collect::<AppResult<Vec<_>>>()?
            .join(" UNION ALL ");
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS {}, * FROM ({selects}) AS appended;",
            quote_identifier(&result),
            quote_identifier(&synthetic)
        );
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, &synthetic)?;
        Ok(result)
    }

    pub(crate) fn grouped_table_workspace(
        &self,
        table: &str,
        keys: &[String],
        aggregations: &[(String, Aggregator)],
    ) -> AppResult<String> {
        if keys.is_empty() || aggregations.is_empty() {
            return Err(AppError::from(
                "Grouping needs at least one key and one aggregation",
            ));
        }
        let headers = self.get_headers(table)?;
        if keys.iter().any(|key| !headers.contains(key))
            || aggregations
                .iter()
                .any(|(column, _)| !headers.contains(column))
        {
            return Err(AppError::from(
                "Grouping column does not exist or is reserved",
            ));
        }
        let mut output_names = HashSet::new();
        let synthetic = Self::synthetic_name(keys);
        let mut select = vec![format!(
            "ROW_NUMBER() OVER () AS {}",
            quote_identifier(&synthetic)
        )];
        for key in keys {
            output_names.insert(key.clone());
            select.push(quote_identifier(key));
        }
        for (column, aggregator) in aggregations {
            let expression = self.aggregate_expression(table, column, *aggregator)?;
            let base = format!("{}_{}", aggregator.name(), column);
            let alias = Self::unique_output_name(base, &mut output_names);
            select.push(format!("{expression} AS {}", quote_identifier(&alias)));
        }
        let group_by = keys
            .iter()
            .map(|key| quote_identifier(key))
            .collect::<Vec<_>>()
            .join(", ");
        let result = self.find_unused_table_name(&format!("{table}_grouped"))?;
        let query = format!(
            "CREATE TABLE {} AS SELECT {} FROM {} GROUP BY {};",
            quote_identifier(&result),
            select.join(", "),
            quote_identifier(table),
            group_by
        );
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, &synthetic)?;
        Ok(result)
    }

    pub(crate) fn pivot_table_workspace(
        &self,
        table: &str,
        row_keys: &[String],
        pivot_column: &str,
        aggregations: &[(String, Aggregator)],
    ) -> AppResult<String> {
        if row_keys.is_empty() || aggregations.is_empty() {
            return Err(AppError::from(
                "Pivot needs at least one row key and one aggregation",
            ));
        }
        let headers = self.get_headers(table)?;
        if row_keys.iter().any(|key| !headers.contains(key))
            || !headers.contains(&pivot_column.to_owned())
            || aggregations
                .iter()
                .any(|(column, _)| !headers.contains(column))
        {
            return Err(AppError::from("Pivot column does not exist or is reserved"));
        }
        let mut statement = self.connection.prepare(&format!(
            "SELECT DISTINCT {} FROM {} ORDER BY {}",
            quote_identifier(pivot_column),
            quote_identifier(table),
            quote_identifier(pivot_column)
        ))?;
        let categories = statement
            .query_map([], |row| Ok(DataItem::from(row.get_ref(0)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        if categories.len().saturating_mul(aggregations.len()) > 1_000 {
            return Err(AppError::from(
                "Pivot would create more than 1,000 columns; filter the source first",
            ));
        }
        let mut output_names = HashSet::new();
        let synthetic = Self::synthetic_name(row_keys);
        let mut select = vec![format!(
            "ROW_NUMBER() OVER () AS {}",
            quote_identifier(&synthetic)
        )];
        for key in row_keys {
            output_names.insert(key.clone());
            select.push(quote_identifier(key));
        }
        for category in &categories {
            let literal = Self::sql_literal(category);
            let condition = format!("{} IS {literal}", quote_identifier(pivot_column));
            for (column, aggregator) in aggregations {
                let aggregate = self.aggregate_expression_with_condition(
                    table,
                    column,
                    *aggregator,
                    Some(&condition),
                )?;
                let base = format!("{}_{}_{}", aggregator.name(), column, category);
                let alias = Self::unique_output_name(base, &mut output_names);
                select.push(format!(
                    "CASE WHEN SUM(CASE WHEN {condition} THEN 1 ELSE 0 END) = 0 THEN NULL ELSE {aggregate} END AS {}",
                    quote_identifier(&alias)
                ));
            }
        }
        let group_by = row_keys
            .iter()
            .map(|key| quote_identifier(key))
            .collect::<Vec<_>>()
            .join(", ");
        let result = self.find_unused_table_name(&format!("{table}_pivot"))?;
        let query = format!(
            "CREATE TABLE {} AS SELECT {} FROM {} GROUP BY {};",
            quote_identifier(&result),
            select.join(", "),
            quote_identifier(table),
            group_by
        );
        self.execute_workspace_batch(&query)?;
        self.mark_synthetic_column(&result, &synthetic)?;
        Ok(result)
    }

    fn aggregate_expression(
        &self,
        table: &str,
        column: &str,
        aggregator: Aggregator,
    ) -> AppResult<String> {
        self.aggregate_expression_with_condition(table, column, aggregator, None)
    }

    fn aggregate_expression_with_condition(
        &self,
        table: &str,
        column: &str,
        aggregator: Aggregator,
        condition: Option<&str>,
    ) -> AppResult<String> {
        let quoted = quote_identifier(column);
        let declared = self.column_declared_type(table, column)?;
        let numeric = Self::type_affinity(&declared) == "numeric";
        if matches!(aggregator, Aggregator::Sum | Aggregator::Average) && !numeric {
            return Err(AppError::from("Sum and average require a numeric column"));
        }
        let value = if numeric {
            format!("CASE WHEN typeof({quoted}) IN ('integer','real') THEN {quoted} END")
        } else {
            quoted.clone()
        };
        let value = if let Some(condition) = condition {
            format!("CASE WHEN {condition} THEN {value} END")
        } else {
            value
        };
        Ok(match aggregator {
            Aggregator::Count => format!("COUNT({value})"),
            Aggregator::DistinctCount => format!("COUNT(DISTINCT {value})"),
            Aggregator::Sum => format!("SUM({value})"),
            Aggregator::Average => format!("AVG({value})"),
            Aggregator::Min => format!("MIN({value})"),
            Aggregator::Max => format!("MAX({value})"),
        })
    }

    fn unique_output_name(base: String, names: &mut HashSet<String>) -> String {
        let mut candidate = base.clone();
        let mut suffix = 1;
        while !names.insert(candidate.clone()) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        candidate
    }

    fn sql_literal(value: &DataItem) -> String {
        match value {
            DataItem::Null => "NULL".into(),
            DataItem::Integer(value) => value.to_string(),
            DataItem::Float(value) => value.to_string(),
            DataItem::Text(value) => format!("'{}'", value.replace('\'', "''")),
            DataItem::Blob(value) => format!(
                "X'{}'",
                value.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
        }
    }

    fn column_declared_type(&self, table: &str, column: &str) -> AppResult<String> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))?;
        let columns = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        columns
            .into_iter()
            .find(|(name, _)| name == column)
            .map(|(_, ty)| ty)
            .ok_or_else(|| AppError::from("Join column does not exist"))
    }

    fn type_affinity(declared_type: &str) -> &'static str {
        let ty = declared_type.to_ascii_uppercase();
        if ty.contains("INT") {
            "numeric"
        } else if ty.contains("CHAR") || ty.contains("CLOB") || ty.contains("TEXT") {
            "text"
        } else if ty.contains("REAL")
            || ty.contains("FLOA")
            || ty.contains("DOUB")
            || ty.contains("NUM")
            || ty.contains("DEC")
        {
            "numeric"
        } else if ty.is_empty() || ty.contains("BLOB") {
            "blob"
        } else {
            "numeric"
        }
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
    pub(crate) fn regex_filter_workspace(
        &self,
        table_name: &str,
        header: &str,
        pattern: &str,
    ) -> AppResult<String> {
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
    ) -> AppResult<Option<u32>> {
        self.source_read(|| {
            self.exact_search_in_snapshot(table_name, ordering, current_row, search_header, pattern)
        })
    }

    fn exact_search_in_snapshot(
        &self,
        table_name: &str,
        ordering: &str,
        current_row: u32,
        search_header: &str,
        pattern: &str,
    ) -> AppResult<Option<u32>> {
        (|| {
            self.check_cancelled()?;
        self.ensure_ordering(table_name, ordering)?;
        let position: Option<u32> = self.connection.query_row(&format!(
            "SELECT p.position FROM temp.__dataman_undo_page_order p JOIN {} t ON t.{} = p.storage_key
             WHERE t.{} = ? AND p.position > ? ORDER BY p.position LIMIT 1",
            quote_identifier(table_name), quote_identifier(&self.storage_key(table_name)?), quote_identifier(search_header)), params![pattern, current_row], |r| r.get(0)).optional()?;
        Ok(position)

        })().map_err(|error: AppError| error.normalize_cancelled(self.cancellation.load(Ordering::Relaxed)).context(format!("Searching sheet '{table_name}'")))
    }

    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.
    pub(crate) fn regex_capture_group_transform_workspace(
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
        self.execute_workspace_batch(&queries)?;
        Ok(())
    }
    pub(crate) fn regex_no_capture_group_transform_workspace(
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
        self.execute_workspace_batch(&queries)?;
        Ok(())
    }

    fn add_expression_column(
        &self,
        table: &str,
        base: &str,
        kind: &str,
        expression: &str,
    ) -> AppResult<()> {
        let column = self.find_unused_header_name(table, base)?;
        let table = quote_identifier(table);
        let column = quote_identifier(&column);
        self.execute_workspace_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}; UPDATE {table} SET {column} = {expression};"))
    }

    pub(crate) fn copy_workspace(&self, table_name: &str, header: &str) -> AppResult<()> {
        self.add_expression_column(
            table_name,
            &format!("{header}_copy"),
            "TEXT",
            &quote_identifier(header),
        )
    }

    pub(crate) fn sql_query_workspace(&self, query: &str) -> AppResult<()> {
        // User transaction statements must not escape the operation's rollback boundary.
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let changed = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let observed = changed.clone();
        let private_tables = {
            let mut statement = self
                .connection
                .prepare("SELECT name FROM __dataman_session.owned_objects WHERE scope='table'")?;
            let tables = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<HashSet<_>, _>>()?;
            tables
        };

        let (private_keys, private_indices, managed_schema) = {
            let mut statement = self.connection.prepare(
                "SELECT scope,name FROM __dataman_session.owned_objects WHERE scope!='table'",
            )?;
            let records = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut managed = HashSet::new();
            let mut keys = HashSet::new();
            let mut indices = HashSet::new();
            for (scope, name) in records {
                for kind in ["column:", "export:", "identity:", "index:"] {
                    if let Some(table) = scope.strip_prefix(kind) {
                        managed.insert(table.to_owned());
                    }
                }

                if scope == "index" {
                    indices.insert(name);
                } else if let Some(table) = scope.strip_prefix("identity:") {
                    keys.insert((table.to_owned(), name));
                }
            }
            (keys, indices, managed)
        };
        let version: i64 = self
            .connection
            .pragma_query_value(None, "schema_version", |row| row.get(0))?;
        let temp_version: i64 = self.connection.pragma_query_value(
            Some(rusqlite::DatabaseName::Temp),
            "schema_version",
            |row| row.get(0),
        )?;
        let (private_functions, private_triggers) = self.identity_trackers();
        let functions_before = self.identity_functions();
        let result = self.atomic(|| {
            self.connection.authorizer(Some(move |context: AuthContext<'_>| {
                match context.action {
                    AuthAction::Insert { table_name } | AuthAction::Delete { table_name } | AuthAction::Update { table_name, .. } if context.database_name == Some("main") => {
                        if let Ok(mut tables) = observed.lock() { tables.insert(table_name.to_owned()); }
                    }
                    _ => {}
                }
                let managed_schema_change = matches!(context.action, AuthAction::AlterTable { table_name, .. } | AuthAction::DropTable { table_name } | AuthAction::CreateTable { table_name } if managed_schema.contains(table_name));
                let private_key_write = match context.action {
                    AuthAction::Update { table_name, column_name } => private_keys.contains(&(table_name.to_owned(),column_name.to_owned())),
                    AuthAction::AlterTable { table_name, .. } => private_keys.iter().any(|(table,_)| table==table_name),
                    AuthAction::CreateIndex { index_name, .. } | AuthAction::DropIndex { index_name, .. } => private_indices.contains(index_name),
                    _ => false,
                };
                let private_write = matches!(context.action, AuthAction::Insert { table_name } | AuthAction::Update { table_name, .. } | AuthAction::Delete { table_name } | AuthAction::CreateTable { table_name } | AuthAction::DropTable { table_name } | AuthAction::AlterTable { table_name, .. } if private_tables.contains(table_name));
                let private_call = matches!(context.action, AuthAction::Function { function_name } if private_functions.contains(function_name) && !context.accessor.is_some_and(|name| private_triggers.contains(name)));
                let own_trigger_drop = matches!(context.action, AuthAction::DropTempTrigger { trigger_name, .. } if private_triggers.contains(trigger_name));
                let temp_schema = context.database_name == Some("temp") && matches!(context.action, AuthAction::Read { table_name: "sqlite_temp_master" | "sqlite_temp_schema", .. } | AuthAction::Delete { table_name: "sqlite_temp_master" | "sqlite_temp_schema" } | AuthAction::Update { table_name: "sqlite_temp_master" | "sqlite_temp_schema", .. });
                if managed_schema_change || private_key_write || private_write || private_call || (matches!(context.database_name, Some("__dataman_session" | "temp")) && !own_trigger_drop && !temp_schema)
                    || matches!(
                        context.action,
                        AuthAction::Transaction { .. }
                            | AuthAction::Savepoint { .. }
                            | AuthAction::Attach { .. }
                            | AuthAction::Detach { .. }
                            | AuthAction::Pragma { .. }
                    )
                {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
            let tracking = self.suspend_identity_tracking();
            let result = self.connection.execute_batch(query).map_err(AppError::from);
            drop(tracking);
            self.connection
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            result?;
            let after: i64 = self.connection.pragma_query_value(None, "schema_version", |row| row.get(0))?;
            let temp_after: i64 = self.connection.pragma_query_value(Some(rusqlite::DatabaseName::Temp), "schema_version", |row| row.get(0))?;
            let tables = changed.lock().map_err(|_| AppError::from("SQL identity tracking failed"))?.clone();
            self.finish_sql_identity_boundary(tables, version != after || temp_version != temp_after)
        });
        if result.is_ok() {
            self.retire_identity_functions(functions_before);
        }
        result
    }

    pub(crate) fn get_table_name(file: PathBuf) -> Option<String> {
        file.file_stem().map(|el| el.to_string_lossy().into_owned())
    }

    pub fn get_headers(&self, table_name: &str) -> AppResult<Vec<String>> {
        if let Some(headers) = self.cache.borrow().headers.get(table_name) {
            return Ok(headers.clone());
        }
        let column_names = super::metadata::visible_columns(&self.connection, table_name, false)?
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        self.cache
            .borrow_mut()
            .headers
            .insert(table_name.to_owned(), column_names.clone());
        Ok(column_names)
    }
    pub fn count_headers(&self, table_name: &str) -> AppResult<u16> {
        Ok(self.get_headers(table_name)?.len() as u16)
    }

    /// Integer-key compatibility helper. Use `edit_row` for opaque row handles
    /// and for editing user primary-key columns.
    pub(crate) fn update_cell_workspace(
        &self,
        table_name: &str,
        header: &str,
        id: i64,
        content: &str,
    ) -> AppResult<()> {
        let key = self.legacy_integer_key(table_name)?;
        if header == "id" {
            return Err(AppError::from("The id column is read-only"));
        }
        let update_query = format!(
            "UPDATE {} SET {} = ? WHERE {} = ?",
            quote_identifier(table_name),
            quote_identifier(header),
            quote_identifier(&key)
        );
        self.atomic_values(table_name, header, || {
            self.connection
                .execute(&update_query, params![content, id])?;
            Ok(())
        })
    }

    pub(crate) fn text_to_int_workspace(&self, table_name: &str, column: &str) -> AppResult<()> {
        self.add_expression_column(
            table_name,
            &format!("INTEGER_{column}"),
            "INTEGER",
            &format!("dataman_try_integer({})", quote_identifier(column)),
        )
    }

    // Compatibility adapter: integer arguments refer to a verified legacy `id`
    // or SQLite rowid. New callers should use the opaque-handle API.
    fn legacy_integer_key(&self, table: &str) -> AppResult<String> {
        if !self.get_headers(table)?.iter().any(|header| header == "id") {
            return self.storage_key(table);
        }
        self.require_integer_identity(table)?;
        Ok("id".to_owned())
    }

    fn require_integer_identity(&self, table: &str) -> AppResult<()> {
        if self.indexed_id_order(table, "")?.is_some() {
            return Ok(());
        }
        if self.get_headers(table)?.iter().any(|header| header == "id") {
            let safe = self.connection.query_row(&format!("SELECT COUNT(*)=COUNT(DISTINCT id) AND NOT EXISTS(SELECT 1 FROM {} WHERE typeof(id)!='integer') FROM {}", quote_identifier(table), quote_identifier(table)), [], |row| row.get::<_, bool>(0))?;
            if safe {
                return Ok(());
            }
        }
        Err(AppError::from("The integer-key compatibility API cannot address this sheet; use open_sheet/fetch_page and opaque RowId handles"))
    }

    pub(crate) fn int_to_text_workspace(&self, table_name: &str, column: &str) -> AppResult<()> {
        self.add_expression_column(
            table_name,
            &format!("TEXT_{column}"),
            "TEXT",
            &format!("CAST({} AS TEXT)", quote_identifier(column)),
        )
    }

    pub(crate) fn delete_column_workspace(
        &self,
        table_name: &str,
        column: &str,
    ) -> AppResult<String> {
        let key = self.connection.query_row(
            "SELECT pk FROM pragma_table_xinfo(?1) WHERE name=?2",
            params![table_name, column],
            |row| row.get::<_, i64>(0),
        )?;
        if key > 0 {
            return Err(AppError::from(
                "Deleting a primary-key column is not supported; it can be renamed or edited",
            ));
        }
        let hidden = self.internal_column_name(table_name)?;
        self.rename_column_workspace(table_name, column, &hidden)?;
        Ok(hidden)
    }

    pub(crate) fn rename_column_workspace(
        &self,
        table_name: &str,
        column: &str,
        new_column: &str,
    ) -> AppResult<()> {
        let queries = sql_queries::build::rename_column_query(table_name, column, new_column);
        self.atomic(|| {
            self.prepare_identity_column_rename(table_name, column, new_column)?;
            self.connection.execute_batch(&queries)?;
            super::metadata::disown(&self.connection, &format!("column:{table_name}"), column)?;
            self.rename_column_identity(table_name, column, new_column)?;
            Ok(())
        })
    }

    pub(crate) fn delete_table_workspace(&self, table_name: &str) -> AppResult<String> {
        let hidden = self.internal_table_name()?;
        self.rename_table_workspace(table_name, &hidden)?;
        log::info!("Deleted table {table_name}");
        Ok(hidden)
    }

    pub(crate) fn delete_tables_workspace(
        &self,
        table_names: &[String],
    ) -> AppResult<Vec<(String, String)>> {
        self.atomic(|| {
            let mut deleted = Vec::with_capacity(table_names.len());
            for table in table_names {
                if table == self.catalog_name()
                    || super::metadata::is_owned(&self.connection, "table", table)?
                {
                    return Err(AppError::from("Protected tables cannot be deleted"));
                }
                let hidden = self.internal_table_name()?;
                self.rename_table_workspace(table, &hidden)?;
                deleted.push((table.clone(), hidden));
            }
            Ok(deleted)
        })
    }

    pub(crate) fn rename_table_workspace(
        &self,
        old_table_name: &str,
        new_table_name: &str,
    ) -> AppResult<()> {
        let query = sql_queries::build::rename_table_query(old_table_name, new_table_name);
        self.atomic(|| {
            self.connection.execute_batch(&query)?;
            super::metadata::disown(&self.connection, "table", old_table_name)?;
            for kind in ["column", "export", "identity", "index"] {
                self.connection.execute(
                    "DELETE FROM __dataman_session.owned_objects WHERE scope=?1",
                    [format!("{kind}:{new_table_name}")],
                )?;
                self.connection.execute(
                    "UPDATE __dataman_session.owned_objects SET scope=?1 WHERE scope=?2",
                    params![
                        format!("{kind}:{new_table_name}"),
                        format!("{kind}:{old_table_name}")
                    ],
                )?;
            }
            self.rename_sheet_identity(old_table_name, new_table_name)?;
            Ok(())
        })
    }

    // TODO if the column contains a float, 3.0, then ensure that ALL intermediary calculations are done with floats.
    // currently (3/2)*2.0 = 2.0, but it should be 3.0.
    pub(crate) fn math_operation_workspace(
        &self,
        table_name: &str,
        inputs: Vec<String>,
    ) -> AppResult<()> {
        let expression = &inputs[0];
        let kind = if expression.contains('.') {
            "REAL"
        } else {
            "INTEGER"
        };
        self.add_expression_column(table_name, "math_expr", kind, expression)
    }

    pub(crate) fn view_metadata_table(&self) -> Result<(), AppError> {
        self.atomic(|| populate_catalog(&self.connection, self.catalog_name()))?;
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
        assert_eq!(number_of_headers, 3)
    }

    #[test]
    fn frequency_table_groups_nulls_and_summary_counts_them() {
        let mut database = setup_database();
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
    fn expression_columns_quote_names_disambiguate_and_roll_back_failures() {
        let mut database = setup_database();
        database.execute_batch(r#"CREATE TABLE "quoted""table" (id INTEGER PRIMARY KEY, "raw""value" TEXT); INSERT INTO "quoted""table" VALUES (1, '42');"#).unwrap();
        let table = "quoted\"table";
        let column = "raw\"value";
        database.copy(table, column).unwrap();
        database.text_to_int(table, column).unwrap();
        database.int_to_text(table, column).unwrap();
        database.int_to_text(table, column).unwrap();
        assert_eq!(
            database
                .get_cell_item(table, 1, "INTEGER_raw\"value")
                .unwrap(),
            DataItem::Integer(42)
        );
        assert_eq!(
            database.get_cell(table, 1, "TEXT_raw\"value_1").unwrap(),
            "42"
        );
        let headers = database.get_headers(table).unwrap();
        assert!(database
            .math_operation(table, vec!["not_a_function()".into()])
            .is_err());
        assert_eq!(database.get_headers(table).unwrap(), headers);
        database
            .rename_column(table, column, "renamed\"column")
            .unwrap();
        database.rename_table(table, "renamed\"table").unwrap();
        assert_eq!(
            database
                .get_cell("renamed\"table", 1, "renamed\"column")
                .unwrap(),
            "42"
        );
    }

    #[test]
    fn text_to_integer_preserves_source_and_sets_invalid_values_to_null() {
        let mut database = setup_database();
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
        let mut database = setup_database();
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
        let mut database = setup_database();
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
        let mut database = setup_database();
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
        let mut database = setup_database();
        database.execute_batch("CREATE TABLE lefts (id INTEGER PRIMARY KEY, key TEXT, name TEXT); INSERT INTO lefts(key,name) VALUES ('x','Ada'), ('y','Grace'); CREATE TABLE rights (id INTEGER PRIMARY KEY, key TEXT, score INTEGER); INSERT INTO rights(key,score) VALUES ('x', 9), ('z', 4);").unwrap();
        let result = database
            .join_tables("lefts", "key", "rights", "key")
            .unwrap();
        assert_eq!(database.count_rows(&result).unwrap(), 1);
        assert_eq!(database.get_cell(&result, 1, "lefts_name").unwrap(), "Ada");
        assert_eq!(database.get_cell(&result, 1, "rights_score").unwrap(), "9");
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
    fn composite_full_join_preserves_duplicates_and_unmatched_rows() {
        let mut database = setup_database();
        database
            .execute_batch("CREATE TABLE lefts (id INTEGER PRIMARY KEY, k1 TEXT, k2 INTEGER, value TEXT); INSERT INTO lefts(k1,k2,value) VALUES ('x',1,'l1'), ('x',1,'l2'), ('y',2,'l3'); CREATE TABLE rights (id INTEGER PRIMARY KEY, k1 TEXT, k2 INTEGER, value TEXT); INSERT INTO rights(k1,k2,value) VALUES ('x',1,'r1'), ('x',1,'r2'), ('z',3,'r3');")
            .unwrap();
        let result = database
            .join_tables_with_keys(
                "lefts",
                "rights",
                &[("k1".into(), "k1".into()), ("k2".into(), "k2".into())],
                JoinKind::Full,
            )
            .unwrap();
        let rows = database
            .fetch_page(database.open_sheet_existing(&result).unwrap(), 10, 0, "")
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows.iter()
                .filter(|row| row[3] == DataItem::Text("l1".into())
                    || row[3] == DataItem::Text("l2".into()))
                .count(),
            4
        );
        assert!(rows
            .iter()
            .any(|row| row[3] == DataItem::Text("l3".into()) && row[7] == DataItem::Null));
        assert!(rows
            .iter()
            .any(|row| row[3] == DataItem::Null && row[7] == DataItem::Text("r3".into())));
    }

    #[test]
    fn append_aligns_columns_by_name_and_assigns_fresh_ids() {
        let mut database = setup_database();
        database
            .execute_batch("CREATE TABLE firsts (id INTEGER PRIMARY KEY, a TEXT, shared INTEGER); INSERT INTO firsts(a,shared) VALUES ('one',1), ('two',2); CREATE TABLE seconds (id INTEGER PRIMARY KEY, shared INTEGER, b TEXT); INSERT INTO seconds(shared,b) VALUES (3,'three'), (4,'four');")
            .unwrap();
        let result = database
            .append_tables(&["firsts".into(), "seconds".into()])
            .unwrap();
        let page = database
            .fetch_page(database.open_sheet_existing(&result).unwrap(), 10, 0, "")
            .unwrap();
        assert_eq!(
            page.columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "a", "shared", "b"]
        );
        assert_eq!(
            page.row_ids.iter().copied().collect::<HashSet<_>>().len(),
            4
        );
        let rows = page.rows;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0][0], DataItem::Integer(1));
        assert_eq!(rows[0][3], DataItem::Null);
        assert_eq!(rows[2][0], DataItem::Integer(1));
        assert_eq!(rows[2][1], DataItem::Null);
        assert_eq!(rows[2][3], DataItem::Text("three".into()));
    }

    #[test]
    fn composite_join_rejects_incompatible_key_affinities() {
        let mut database = setup_database();
        database.execute_batch("CREATE TABLE lefts (id INTEGER PRIMARY KEY, key TEXT); CREATE TABLE rights (id INTEGER PRIMARY KEY, key INTEGER);").unwrap();
        assert!(database
            .join_tables_with_keys(
                "lefts",
                "rights",
                &[("key".into(), "key".into())],
                JoinKind::Inner,
            )
            .is_err());
        assert!(!database
            .get_table_names()
            .unwrap()
            .iter()
            .any(|name| name.contains("join")));
    }

    #[test]
    fn cancelled_append_leaves_no_partial_result() {
        let mut database = setup_database();
        database.execute_batch("CREATE TABLE firsts (id INTEGER PRIMARY KEY, value TEXT); CREATE TABLE seconds (id INTEGER PRIMARY KEY, value TEXT);").unwrap();
        database
            .cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(database
            .append_tables(&["firsts".into(), "seconds".into()])
            .is_err());
        database
            .cancellation
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!database
            .get_table_names()
            .unwrap()
            .contains(&"append".into()));
    }

    #[test]
    fn grouping_and_pivot_aggregate_without_coercing_invalid_numeric_values() {
        let mut database = setup_database();
        database.execute_batch("CREATE TABLE sales (id INTEGER PRIMARY KEY, customer TEXT, month TEXT, amount INTEGER); INSERT INTO sales(customer,month,amount) VALUES ('A','Jan',10), ('A','Feb',20), ('A','Jan',5), ('B','Jan','bad'), ('B',NULL,3);").unwrap();
        let grouped = database
            .grouped_table(
                "sales",
                &["customer".into()],
                &[("amount".into(), Aggregator::Sum)],
            )
            .unwrap();
        let grouped_rows = database.get(10, 0, &grouped, "").unwrap().1;
        let customer_a = grouped_rows
            .iter()
            .find(|row| row[1] == DataItem::Text("A".into()))
            .unwrap();
        assert_eq!(customer_a[2], DataItem::Integer(35));

        let pivot = database
            .pivot_table(
                "sales",
                &["customer".into()],
                "month",
                &[("amount".into(), Aggregator::Sum)],
            )
            .unwrap();
        let headers = database.get_headers(&pivot).unwrap();
        assert!(headers.contains(&"sum_amount_Jan".into()));
        assert!(headers.contains(&"sum_amount_Feb".into()));
        let rows = database.get(10, 0, &pivot, "").unwrap().1;
        let a = rows
            .iter()
            .find(|row| row[1] == DataItem::Text("A".into()))
            .unwrap();
        let jan = headers
            .iter()
            .position(|header| header == "sum_amount_Jan")
            .unwrap();
        let feb = headers
            .iter()
            .position(|header| header == "sum_amount_Feb")
            .unwrap();
        assert_eq!(a[jan], DataItem::Integer(15));
        assert_eq!(a[feb], DataItem::Integer(20));
        let b = rows
            .iter()
            .find(|row| row[1] == DataItem::Text("B".into()))
            .unwrap();
        assert_eq!(b[jan], DataItem::Null);
    }

    #[test]
    fn pivot_rejects_more_than_one_thousand_generated_columns_before_creation() {
        let mut database = setup_database();
        database.execute_batch("CREATE TABLE wide (id INTEGER PRIMARY KEY, row_key TEXT, category INTEGER, amount INTEGER); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < 1001) INSERT INTO wide(row_key,category,amount) SELECT 'one',x,x FROM n;").unwrap();
        assert!(database
            .pivot_table(
                "wide",
                &["row_key".into()],
                "category",
                &[("amount".into(), Aggregator::Sum)]
            )
            .is_err());
        assert!(!database
            .get_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with("wide_pivot")));
    }

    #[test]
    fn derive_column_test() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let col = "firstname";
        let col_new = "firstname_derived";
        let fun = |s| Some(format!("{}-changed", s));
        database.derive_column("data", col, col_new, fun).unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .get(3)
            .unwrap()
            .to_string();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers("data").unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn update_cell() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database
            .update_cell("data", "firstname", 1, "hank")
            .unwrap();
        let first: String = database.get(1, 0, "data", "").unwrap().1[0]
            .first()
            .unwrap()
            .to_string();

        let (_, rows) = database.get(1, 0, "data", "").unwrap();

        assert_eq!(first, "hank");
    }

    #[test]
    fn update_cell_rejects_the_id_column() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
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
        let mut database = Database::new(connection).unwrap();
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
        let mut database = Database::new(connection).unwrap();
        let filtered = database.regex_filter("t", "name", "'b").unwrap();
        assert_eq!(database.count_rows(&filtered).unwrap(), 1);
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
        let mut database = Database::new(connection).unwrap();
        let filtered = database.regex_filter("t", "name", "a").unwrap();
        assert_eq!(database.count_rows(&filtered).unwrap(), 1);
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
        let mut database = Database::new(connection).unwrap();
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
        let mut database = Database::new(connection).unwrap();
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
        let mut database = Database::new(connection).unwrap();
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
        let mut database = Database::new(connection).unwrap();
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
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();

        // we use limit 2 so we can check first and second item
        let (headers, data_rows) = database.get(2, 0, "data", "").unwrap();
        let first: String = data_rows[0].first().unwrap().to_string();
        assert_eq!("henrik", first);

        // we can look into the old data rows to see the next item:
        let second: String = data_rows[1].first().unwrap().to_string();
        assert_eq!("john", second);

        // or we can update the 'cache'
        // this makes us get a new view into the database
        // if we don't use this we would need to index into [1] instead
        let (headers, data_rows) = database.get(1, 1, "data", "").unwrap();
        let second: String = data_rows[0].first().unwrap().to_string();
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
        assert_eq!(headers, vec!["firstname", "lastname", "age"]);
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
    fn __dataman_catalog_test() {
        let database = setup_three_table_db();
    }

    #[test]
    fn copy_column_long_test() {
        let p = vec![PathBuf::from("assets/data-long.csv")];
        let mut database = Database::try_from(p).unwrap();

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
            let original = row.first();
            let copy = row.get(3);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_test() {
        let p = vec![PathBuf::from("assets/data.csv")];
        let mut database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        let column_name = "firstname";
        let new_column_name = database
            .find_unused_header_name("data", column_name)
            .unwrap();
        database
            .derive_column("data", column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 0, "data", "").unwrap();
        for row in res.iter() {
            let original = row.first();
            let copy = row.get(3);
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
        let mut first = Database::try_from(vec![source.clone()]).unwrap();
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
        let mut database = database(100);
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
        let mut database = database(3);
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
        let mut database = database(3);
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
        let mut database = database(3);
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
        assert_eq!(database.count_rows("t").unwrap(), 1);
        database.toggle_history_record(&mut record).unwrap();
        assert_eq!(database.count_rows("t").unwrap(), 3);
    }

    #[test]
    fn regex_selection_and_selected_copy_reach_beyond_one_million_rows() {
        let mut database = database(0);
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
        let mut database = database(0);
        database.execute_batch("INSERT INTO t VALUES (3,'same'),(1,NULL),(2,'same'),(4,'z'); CREATE TABLE derived AS SELECT * FROM t;").unwrap();
        for table in ["t", "derived"] {
            let mut ids = |order: &str| {
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
                Some(2)
            );
        }
        // The derived table has no id index: page retrieval must use its storage rowid.
        let plan: String = database.connection.query_row("EXPLAIN QUERY PLAN SELECT t.* FROM temp.__dataman_undo_page_order p JOIN derived t ON t.rowid=p.storage_key WHERE p.position>=2 AND p.position<3 ORDER BY p.position", [], |r| r.get(3)).unwrap();
        assert!(plan.contains("INTEGER PRIMARY KEY"), "{plan}");
    }

    #[test]
    fn cancelled_order_rebuild_does_not_reuse_an_incomplete_map() {
        let mut database = database(1000);
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
        let mut controller =
            crate::controller::controller_impl::Controller::new(database(10)).unwrap();
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
        let mut database = database(3);
        database.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON t WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
        assert!(database
            .delete_selected("t", &HashSet::from([1, 2, 3]))
            .is_err());
        assert_eq!(database.count_rows("t").unwrap(), 3);
        assert!(!database
            .raw_table_names()
            .unwrap()
            .iter()
            .any(|name| name.starts_with(INTERNAL_PREFIX)));
    }

    #[test]
    fn cancelled_derivation_rolls_back_the_added_column() {
        let mut database = database(10);
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
        let mut database = database(3);
        assert!(database
            .sql_query("UPDATE t SET value='changed'; COMMIT;")
            .is_err());
        assert_eq!(database.get_cell("t", 1, "value").unwrap(), "v1");
        assert!(database.connection.is_autocommit());
    }

    #[test]
    fn layout_changes_share_cached_cells_without_querying_sqlite() {
        use crate::controller::Command;
        let mut controller =
            crate::controller::controller_impl::Controller::new(database(10)).unwrap();
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
        let mut database = database(3);
        database.execute_batch("CREATE TRIGGER add_row AFTER UPDATE ON t BEGIN INSERT INTO t(value) VALUES('trigger'); END;").unwrap();
        database.get(10, 0, "t", "ORDER BY id").unwrap();
        assert_eq!(database.count_rows("t").unwrap(), 3);
        database.update_cell("t", "value", 1, "changed").unwrap();
        assert_eq!(database.count_rows("t").unwrap(), 4);
        assert_eq!(database.get(10, 0, "t", "ORDER BY id").unwrap().1.len(), 4);
    }

    #[test]
    fn changing_values_keeps_the_id_order_map_and_row_count_cache() {
        let mut database = database(10);
        database.get(10, 0, "t", "ORDER BY id").unwrap();
        database.count_rows("t").unwrap();
        database.update_cell("t", "value", 1, "changed").unwrap();
        assert!(database.cache.borrow().ordering.is_none());
        assert!(database.cache.borrow().page_cursor.is_some());
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
    fn indexed_first_page_and_keyset_scrolling_never_build_position_maps() {
        let mut database = database(0);
        database.execute_batch("INSERT INTO t VALUES (-9223372036854775808, 'min'),(-1,'negative'),(4,'gap'),(9223372036854775807,'max');").unwrap();
        for (order, expected) in [
            ("ORDER BY id ASC", vec![i64::MIN, -1, 4, i64::MAX]),
            ("ORDER BY id DESC", vec![i64::MAX, 4, -1, i64::MIN]),
        ] {
            let mut ids = Vec::new();
            for offset in [0, 2] {
                let page = database.get(2, offset, "t", order).unwrap();
                for row in page.1 {
                    let DataItem::Integer(id) = row[0] else {
                        panic!("unexpected identity")
                    };
                    ids.push(id);
                }
            }
            assert_eq!(ids, expected);
        }
        assert!(database.cache.borrow().ordering.is_none());
        assert_eq!(database.connection.query_row("SELECT COUNT(*) FROM sqlite_temp_master WHERE name='__dataman_undo_page_order'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn nullable_primary_key_indexes_use_unique_storage_locators() {
        let mut database = database(0);
        database.execute_batch("CREATE TABLE nullable(id INTEGER PRIMARY KEY DESC, value TEXT); INSERT INTO nullable VALUES(NULL,'one'),(NULL,'two'),(3,'three');").unwrap();
        assert_eq!(
            database
                .indexed_id_order("nullable", "ORDER BY id ASC")
                .unwrap(),
            None
        );
        assert_eq!(
            database
                .get(10, 0, "nullable", "ORDER BY id ASC")
                .unwrap()
                .1
                .len(),
            3
        );
        assert!(database
            .update_cell("nullable", "value", 3, "ambiguous")
            .is_err());
    }

    #[test]
    fn cancelled_sqlite_open_preserves_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.db");
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO t VALUES(1,'original');").unwrap();
        let before = std::fs::read(&path).unwrap();
        let token = Arc::new(AtomicBool::new(true));
        assert!(Database::load_cancellable(vec![path.clone()], token)
            .unwrap_err()
            .is_cancelled());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn sql_cannot_attach_external_databases_or_change_connection_settings() {
        let mut database = database(3);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("external.db");
        let sql = format!(
            "UPDATE t SET value='changed'; ATTACH '{}' AS external",
            path.display().to_string().replace('\'', "''")
        );
        assert!(database.sql_query(&sql).is_err());
        assert!(!path.exists());
        assert_ne!(database.get_cell("t", 1, "value").unwrap(), "changed");
        assert!(database.sql_query("PRAGMA writable_schema=ON").is_err());
        assert!(database
            .sql_query("DELETE FROM __dataman_session.owned_objects")
            .is_err());
        assert!(database
            .sql_query("DROP TABLE temp.__dataman_catalog")
            .is_err());
        assert!(database.sql_query("VACUUM INTO 'external.db'").is_err());
        assert_eq!(database.count_rows("t").unwrap(), 3);
    }

    #[test]
    fn saves_cannot_replace_the_open_source_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.db");
        let database = Database::new(Connection::open(&path).unwrap()).unwrap();
        assert!(database.backup_db(&path).is_err());
        assert!(database
            .export_delimited("__dataman_catalog", &path, b',')
            .is_err());
    }

    #[test]
    fn pages_support_without_rowid_tables_and_quoted_sort_columns() {
        let mut database = database(0);
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
    #[test]
    fn search_distinguishes_absence_errors_and_cancellation() {
        let database = database(3);
        assert_eq!(
            database
                .exact_search("t", "", 0, "value", "absent")
                .unwrap(),
            None
        );
        assert!(database
            .exact_search("t", "", 0, "missing_column", "absent")
            .is_err());
        database.cancellation.store(true, Ordering::Relaxed);
        assert!(database
            .exact_search("t", "", 0, "value", "absent")
            .unwrap_err()
            .is_cancelled());
    }

    #[test]
    fn row_count_and_storage_key_propagate_database_failures() {
        use rusqlite::hooks::{AuthContext, Authorization};
        let database = database(3);
        assert!(database.count_rows("missing_table").is_err());
        database
            .connection
            .authorizer(Some(|_: AuthContext<'_>| Authorization::Deny));
        assert!(database.count_rows("t").is_err());
        assert!(database.storage_key("t").is_err());
    }

    #[test]
    fn rollback_failure_preserves_primary_operation_error() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let database = database(3);
        let error = database
            .atomic::<()>(|| {
                database.connection.authorizer(Some(|ctx: AuthContext<'_>| {
                    if matches!(
                        ctx.action,
                        AuthAction::Transaction {
                            operation: rusqlite::hooks::TransactionOperation::Rollback
                        }
                    ) {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }));
                Err(AppError::invalid_input("primary operation failed"))
            })
            .unwrap_err();
        let AppError::Cleanup { source, cleanup } = error else {
            panic!("missing rollback failure")
        };
        assert_eq!(source.to_string(), "primary operation failed");
        assert!(matches!(*cleanup, AppError::Sqlite(_)));
        database
            .connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        database.connection.execute_batch("ROLLBACK").unwrap();
    }
}
