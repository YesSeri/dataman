//! Runtime identities are independent of data columns and row positions.
use super::*;
use std::sync::atomic::AtomicU64;

static SESSION: AtomicU64 = AtomicU64::new(1);

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name {
            session: u64,
            key: i64,
        }
    };
}
identity!(SheetId);
identity!(ColumnId);
identity!(RowId);

#[derive(Debug, Clone)]
pub struct ColumnMetadata {
    pub id: ColumnId,
    pub name: String,
    pub declared_type: String,
    pub writable: bool,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub has_more: bool,
    pub sheet: SheetId,
    pub columns: Vec<ColumnMetadata>,
    pub row_ids: Vec<RowId>,
    pub rows: Vec<Vec<DataItem>>,
}

#[derive(Debug, Clone)]
struct Locator {
    columns: Vec<String>,
    collations: Vec<String>,
    integer: bool,
    alias: Option<String>,
}

impl Locator {
    fn projection(&self, alias: &str) -> String {
        self.columns
            .iter()
            .map(|name| format!("{alias}{}", quote_identifier(name)))
            .collect::<Vec<_>>()
            .join(",")
    }
    fn predicate(&self, alias: &str, start: usize) -> String {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{alias}{} IS ?{}", quote_identifier(name), i + start))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
    fn map_predicate(&self, start: usize) -> String {
        (0..self.columns.len())
            .map(|i| format!("k{i} IS ?{}", i + start))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

#[derive(Debug, Clone)]
struct Sheet {
    id: SheetId,
    name: String,
    locator: Locator,
    tracker: String,
    triggers: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IdentityRegistry {
    session: u64,
    next: i64,
    sheets: HashMap<String, Sheet>,
    columns: HashMap<(SheetId, String), ColumnId>,
    orders: HashMap<SheetId, String>,
    cursors: HashMap<(SheetId, String), (u32, Vec<Value>)>,
    snapshots: HashMap<String, String>,
    memberships: HashMap<String, (SheetId, SheetId)>,
    last_sql_changes: HashSet<String>,
    tracking_enabled: Arc<AtomicBool>,
}

impl IdentityRegistry {
    pub(super) fn new() -> Self {
        Self {
            session: SESSION.fetch_add(1, Ordering::Relaxed),
            next: 0,
            sheets: HashMap::new(),
            columns: HashMap::new(),
            orders: HashMap::new(),
            cursors: HashMap::new(),
            snapshots: HashMap::new(),
            memberships: HashMap::new(),
            last_sql_changes: HashSet::new(),
            tracking_enabled: Arc::new(AtomicBool::new(true)),
        }
    }
    pub(super) fn reset_source(&mut self) {
        let next = self.next;
        *self = Self::new();
        // Sidecar table names contain monotonically allocated sheet keys.
        self.next = next;
    }
    fn allocate(&mut self) -> AppResult<i64> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| AppError::from("Session identity space exhausted"))?;
        Ok(self.next)
    }
    pub(super) fn invalidate_orders(&mut self) {
        self.orders.clear();
        self.cursors.clear();
    }
}

fn map_name(sheet: SheetId) -> String {
    format!("__dataman_session.rows_{}", sheet.key)
}

pub(super) struct TrackingGuard(Arc<AtomicBool>);
impl Drop for TrackingGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Database {
    pub fn edit_row(
        &mut self,
        id: SheetId,
        row: RowId,
        column: ColumnId,
        content: &str,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.edit_row_workspace(id, row, column, content)
    }
    pub(crate) fn bulk_edit_rows(
        &mut self,
        id: SheetId,
        column: ColumnId,
        rows: &HashSet<RowId>,
        value: &str,
    ) -> AppResult<UndoRecord> {
        self.ensure_workspace()?;
        self.bulk_edit_rows_workspace(id, column, rows, value)
    }
    pub(crate) fn delete_rows(
        &mut self,
        id: SheetId,
        rows: &HashSet<RowId>,
    ) -> AppResult<UndoRecord> {
        self.ensure_workspace()?;
        self.delete_rows_workspace(id, rows)
    }
    pub(crate) fn copy_rows(
        &mut self,
        id: SheetId,
        target: &str,
        ordering: &str,
        rows: &HashSet<RowId>,
    ) -> AppResult<()> {
        self.ensure_workspace()?;
        self.copy_rows_workspace(id, target, ordering, rows)
    }

    /// Open a table, or a private snapshot when the source has no usable key.
    pub fn open_sheet(&mut self, name: &str) -> AppResult<SheetId> {
        self.validate_source()?;
        // Materialization requires promotion before entering any transaction.
        if self.storage_state == StorageState::ReadOnlySource {
            let needs_snapshot = self.source_read(|| {
                let canonical: String = self.connection.query_row("SELECT name FROM sqlite_temp_master WHERE name=?1 COLLATE NOCASE AND type IN ('table','view') UNION ALL SELECT name FROM sqlite_master WHERE name=?1 COLLATE NOCASE AND type IN ('table','view') LIMIT 1", [name], |row| row.get(0))?;
                Ok(self.discover_locator(&canonical)?.is_none())
            })?;
            if needs_snapshot {
                self.ensure_workspace()?;
            }
        }
        self.open_sheet_existing(name)
    }

    pub(crate) fn open_sheet_existing(&self, name: &str) -> AppResult<SheetId> {
        self.source_read(|| self.open_sheet_in_snapshot(name))
    }

    fn open_sheet_in_snapshot(&self, name: &str) -> AppResult<SheetId> {
        self.check_cancelled()?;
        if let Some(sheet) = self.identities.borrow().sheets.get(name) {
            return Ok(sheet.id);
        }
        let canonical: String = self.connection.query_row("SELECT name FROM sqlite_temp_master WHERE name=?1 COLLATE NOCASE AND type IN ('table','view') UNION ALL SELECT name FROM sqlite_master WHERE name=?1 COLLATE NOCASE AND type IN ('table','view') LIMIT 1", [name], |row| row.get(0))?;
        let name = canonical.as_str();
        if let Some(sheet) = self.identities.borrow().sheets.get(name) {
            return Ok(sheet.id);
        }
        self.atomic_impl(|| self.open_sheet_storage(name), false)
    }

    fn open_sheet_storage(&self, name: &str) -> AppResult<SheetId> {
        let snapshot = self.identities.borrow().snapshots.get(name).cloned();
        if let Some(snapshot) = snapshot {
            return self.open_sheet_existing(&snapshot);
        }
        self.connection
            .prepare(&format!("SELECT * FROM {} LIMIT 0", quote_identifier(name)))?;
        let locator = if let Some(locator) = self.discover_locator(name)? {
            locator
        } else {
            let snapshot = self.snapshot_sheet(name)?;
            self.identities
                .borrow_mut()
                .snapshots
                .insert(name.to_owned(), snapshot.clone());
            return self.open_sheet_existing(&snapshot);
        };
        let (id, session) = {
            let mut registry = self.identities.borrow_mut();
            (registry.allocate()?, registry.session)
        };
        let id = SheetId { session, key: id };
        let definitions = locator
            .collations
            .iter()
            .enumerate()
            .map(|(i, collation)| format!("k{i} COLLATE {}", quote_identifier(collation)))
            .collect::<Vec<_>>()
            .join(",");
        let keys = (0..locator.columns.len())
            .map(|i| format!("k{i}"))
            .collect::<Vec<_>>()
            .join(",");
        self.connection.execute_batch(&format!("CREATE TABLE {} (handle INTEGER PRIMARY KEY, active INTEGER NOT NULL DEFAULT 1,{definitions}); CREATE UNIQUE INDEX __dataman_session.row_keys_{} ON rows_{} ({keys}) WHERE active=1;", map_name(id), id.key, id.key))?;
        let (tracker, triggers) = self.install_identity_tracking(id, name, &locator)?;
        self.identities.borrow_mut().sheets.insert(
            name.to_owned(),
            Sheet {
                id,
                name: name.to_owned(),
                locator,
                tracker,
                triggers,
            },
        );
        Ok(id)
    }

    pub fn sheet_name(&self, id: SheetId) -> AppResult<String> {
        Ok(self.sheet(id)?.name)
    }

    fn sheet(&self, id: SheetId) -> AppResult<Sheet> {
        self.identities
            .borrow()
            .sheets
            .values()
            .find(|sheet| sheet.id == id)
            .cloned()
            .ok_or_else(|| AppError::from("Sheet handle is stale or belongs to another session"))
    }

    fn ensure_visible_sheet(&self, sheet: &Sheet, mutation: bool) -> AppResult<()> {
        if super::super::metadata::is_owned(&self.connection, "table", &sheet.name)? {
            return Err(AppError::from(
                "This sheet was deleted; undo its deletion before using its rows",
            ));
        }
        if mutation && sheet.name == self.catalog_name() {
            return Err(AppError::from("The sheet catalog is read-only"));
        }
        Ok(())
    }

    pub fn column_id(&self, sheet: SheetId, name: &str) -> AppResult<ColumnId> {
        let table = self.sheet_name(sheet)?;
        let name = self
            .get_headers(&table)?
            .into_iter()
            .find(|header| header.eq_ignore_ascii_case(name))
            .ok_or_else(|| AppError::from("Column no longer exists"))?;
        let mut registry = self.identities.borrow_mut();
        if let Some(id) = registry.columns.get(&(sheet, name.to_owned())) {
            return Ok(*id);
        }
        let id = ColumnId {
            session: registry.session,
            key: registry.allocate()?,
        };
        registry.columns.insert((sheet, name.to_owned()), id);
        Ok(id)
    }

    pub(super) fn data_columns(&self, table: &str) -> AppResult<Vec<(String, String)>> {
        super::super::metadata::visible_columns(&self.connection, table, true)
    }

    pub(super) fn mark_synthetic_column(&self, table: &str, column: &str) -> AppResult<()> {
        super::super::metadata::own(&self.connection, &format!("export:{table}"), column)
    }

    pub(super) fn synthetic_name(columns: &[String]) -> String {
        let mut name = "id".to_owned();
        if columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(&name))
        {
            name = "__dataman_identity".to_owned();
        }
        while columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(&name))
        {
            name.push('_');
        }
        name
    }

    pub(super) fn derive_by_locator<F>(
        &self,
        table: &str,
        old: &str,
        new: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        let sheet = self.sheet(self.open_sheet_existing(table)?)?;
        if sheet.locator.integer && sheet.locator.columns[0].eq_ignore_ascii_case(new) {
            return Err(AppError::from(
                "This derived column would shadow a row locator; choose another column name",
            ));
        }
        self.atomic(|| {
            self.connection.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} TEXT", quote_identifier(&sheet.name), quote_identifier(new)))?;
            let projection = sheet.locator.projection("");
            let mut after: Option<Vec<Value>> = None;
            loop {
                self.check_cancelled()?;
                let query = if after.is_some() {
                    let placeholders = (0..sheet.locator.columns.len()).map(|_| "?").collect::<Vec<_>>().join(",");
                    format!("SELECT {projection},{} FROM {} WHERE ({projection})>({placeholders}) ORDER BY {projection} LIMIT 256", quote_identifier(old), quote_identifier(&sheet.name))
                } else { format!("SELECT {projection},{} FROM {} ORDER BY {projection} LIMIT 256", quote_identifier(old), quote_identifier(&sheet.name)) };
                let batch = {
                    let mut statement = self.connection.prepare(&query)?;
                    let params = after.clone().unwrap_or_default();
                    let data = statement.query_map(rusqlite::params_from_iter(params), |row| Ok(((0..sheet.locator.columns.len()).map(|i| row.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>()?, DataItem::from(row.get_ref(sheet.locator.columns.len())?))))?.collect::<Result<Vec<_>, _>>()?;
                    data
                };
                if batch.is_empty() { break; }
                for (keys, item) in batch {
                    self.check_cancelled()?;
                    let value = if item == DataItem::Null { None } else { fun(item.to_string()) };
                    let params = std::iter::once(value.map_or(Value::Null, Value::Text)).chain(keys.clone()).collect::<Vec<_>>();
                    self.connection.execute(&format!("UPDATE {} SET {}=?1 WHERE {}", quote_identifier(&sheet.name), quote_identifier(new), sheet.locator.predicate("", 2)), rusqlite::params_from_iter(params))?;
                    after = Some(keys);
                }
            }
            Ok(())
        })
    }

    pub fn column_name(&self, sheet: SheetId, id: ColumnId) -> AppResult<String> {
        self.sheet(sheet)?;
        let name = self
            .identities
            .borrow()
            .columns
            .iter()
            .find(|((owner, _), column)| *owner == sheet && **column == id)
            .map(|((_, name), _)| name.clone())
            .ok_or_else(|| AppError::from("Column handle is stale or belongs to another sheet"))?;
        if !self.get_headers(&self.sheet_name(sheet)?)?.contains(&name) {
            return Err(AppError::from("Column handle is stale"));
        }
        Ok(name)
    }

    fn discover_locator(&self, table: &str) -> AppResult<Option<Locator>> {
        let kind = self.connection.query_row("SELECT type FROM sqlite_master WHERE name=?1 UNION ALL SELECT type FROM sqlite_temp_master WHERE name=?1 LIMIT 1", [table], |row| row.get::<_, String>(0))?;
        if kind != "table" {
            return Ok(None);
        }
        let virtual_table = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE name=?1 AND type='virtual')",
            [table],
            |row| row.get::<_, bool>(0),
        )?;
        if virtual_table {
            return Ok(None);
        }
        let private = self
            .connection
            .query_row(
                "SELECT name FROM __dataman_session.owned_objects WHERE scope=?1 LIMIT 1",
                [format!("identity:{table}")],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(private) = private {
            return Ok(Some(Locator {
                columns: vec![private],
                collations: vec!["BINARY".into()],
                integer: false,
                alias: None,
            }));
        }
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_xinfo({})", quote_identifier(table)))?;
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
        for candidate in ["rowid", "_rowid_", "oid"] {
            if columns
                .iter()
                .any(|(name, ..)| name.eq_ignore_ascii_case(candidate))
            {
                continue;
            }
            if self
                .connection
                .prepare(&format!(
                    "SELECT {candidate} FROM {} LIMIT 0",
                    quote_identifier(table)
                ))
                .is_ok()
            {
                let alias = if !self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1) WHERE origin='pk')",
                    [table],
                    |row| row.get::<_, bool>(0),
                )? {
                    columns
                        .iter()
                        .find(|(_, kind, pk, _)| *pk == 1 && kind.eq_ignore_ascii_case("INTEGER"))
                        .map(|(name, ..)| name.clone())
                } else {
                    None
                };
                return Ok(Some(Locator {
                    columns: vec![candidate.to_owned()],
                    collations: vec!["BINARY".into()],
                    integer: true,
                    alias,
                }));
            }
        }
        let mut keys = columns
            .into_iter()
            .filter(|(_, _, pk, _)| *pk > 0)
            .collect::<Vec<_>>();
        keys.sort_by_key(|(_, _, pk, _)| *pk);
        if keys.len() == 1
            && keys[0].1.eq_ignore_ascii_case("INTEGER")
            && !self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_index_list(?1) WHERE origin='pk')",
                [table],
                |row| row.get::<_, bool>(0),
            )?
        {
            let name = keys[0].0.clone();
            return Ok(Some(Locator {
                columns: vec![name.clone()],
                collations: vec!["BINARY".into()],
                integer: false,
                alias: Some(name),
            }));
        }
        if keys.is_empty() || keys.len() > 60 || keys.iter().any(|(_, _, _, not_null)| !not_null) {
            return Ok(None);
        }
        let index: String = self.connection.query_row(
            "SELECT name FROM pragma_index_list(?1) WHERE origin='pk'",
            [table],
            |row| row.get(0),
        )?;
        let mut statement = self
            .connection
            .prepare("SELECT coll FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno")?;
        let collations = statement
            .query_map([index], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Locator {
            columns: keys.into_iter().map(|(name, ..)| name).collect(),
            collations,
            integer: false,
            alias: None,
        }))
    }

    fn snapshot_sheet(&self, source: &str) -> AppResult<String> {
        let name = self.find_unused_table_name(&format!("{source}_snapshot"))?;
        let columns = self.get_headers(source)?;
        let mut identity = "__dataman_identity".to_owned();
        while columns
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&identity))
        {
            identity.push('_');
        }
        let select = columns
            .iter()
            .map(|name| quote_identifier(name))
            .collect::<Vec<_>>()
            .join(",");
        let needs_private = ["rowid", "_rowid_", "oid"].iter().all(|alias| {
            columns
                .iter()
                .any(|column| column.eq_ignore_ascii_case(alias))
        });
        self.atomic(|| {
            if needs_private {
                let definitions = self.data_columns(source)?.iter().map(|(column, kind)| format!("{} {}", quote_identifier(column), quote_identifier(kind))).collect::<Vec<_>>().join(",");
                let index = self.find_unused_table_name("__dataman_identity_index")?;
                self.connection.execute_batch(&format!("CREATE TABLE {} ({} INTEGER NOT NULL,{definitions}); CREATE UNIQUE INDEX {} ON {} ({}); INSERT INTO {} ({},{select}) SELECT ROW_NUMBER() OVER (),{select} FROM {};", quote_identifier(&name), quote_identifier(&identity), quote_identifier(&index), quote_identifier(&name), quote_identifier(&identity), quote_identifier(&name), quote_identifier(&identity), quote_identifier(source)))?;
                super::super::metadata::own(&self.connection, "index", &index)?;
                super::super::metadata::own(&self.connection, &format!("index:{name}"), &index)?;
                super::super::metadata::own(&self.connection, &format!("column:{name}"), &identity)?;
                super::super::metadata::own(&self.connection, &format!("export:{name}"), &identity)?;
                super::super::metadata::own(&self.connection, &format!("identity:{name}"), &identity)?;
            } else {
                self.connection.execute_batch(&format!("CREATE TABLE {} AS SELECT {select} FROM {};", quote_identifier(&name), quote_identifier(source)))?;
            }
            Ok(())
        })?;
        self.invalidate();
        Ok(name)
    }

    fn intern_row(&self, sheet: &Sheet, keys: &[Value]) -> AppResult<RowId> {
        let map = map_name(sheet.id);
        let existing = self
            .connection
            .query_row(
                &format!(
                    "SELECT handle FROM {map} WHERE active=1 AND {}",
                    sheet.locator.map_predicate(1)
                ),
                rusqlite::params_from_iter(keys),
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let key = if let Some(key) = existing {
            key
        } else {
            let key = self.identities.borrow_mut().allocate()?;
            let values = std::iter::once(Value::Integer(key))
                .chain(keys.iter().cloned())
                .collect::<Vec<_>>();
            let columns = (0..keys.len())
                .map(|i| format!("k{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = (0..values.len()).map(|_| "?").collect::<Vec<_>>().join(",");
            self.connection.execute(
                &format!("INSERT INTO {map}(handle,{columns}) VALUES({placeholders})"),
                rusqlite::params_from_iter(values),
            )?;
            key
        };
        Ok(RowId {
            session: sheet.id.session,
            key,
        })
    }

    fn row_locator(&self, sheet: &Sheet, row: RowId, active: bool) -> AppResult<Vec<Value>> {
        if row.session != sheet.id.session {
            return Err(AppError::from("Row handle belongs to another session"));
        }
        let columns = (0..sheet.locator.columns.len())
            .map(|i| format!("k{i}"))
            .collect::<Vec<_>>()
            .join(",");
        self.connection
            .query_row(
                &format!(
                    "SELECT {columns} FROM {} WHERE handle=?1 {}",
                    map_name(sheet.id),
                    if active { "AND active=1" } else { "" }
                ),
                [row.key],
                |row| {
                    (0..sheet.locator.columns.len())
                        .map(|i| row.get(i))
                        .collect()
                },
            )
            .optional()?
            .ok_or_else(|| {
                AppError::from("Row handle is stale, deleted, or belongs to another sheet")
            })
    }

    /// Fetch visible values and row handles together; identity is never a data column.
    pub fn fetch_page(
        &self,
        id: SheetId,
        limit: u32,
        offset: u32,
        ordering: &str,
    ) -> AppResult<Page> {
        self.source_read(|| self.fetch_page_in_snapshot(id, limit, offset, ordering))
    }

    fn fetch_page_in_snapshot(
        &self,
        id: SheetId,
        limit: u32,
        offset: u32,
        ordering: &str,
    ) -> AppResult<Page> {
        self.check_cancelled()?;
        let requested_limit = limit;
        let limit = u64::from(limit) + 1;
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, false)?;
        let visible = super::super::metadata::visible_columns(&self.connection, &sheet.name, true)?;
        let mut columns = Vec::new();
        for (name, kind) in visible {
            let hidden: i64 = self.connection.query_row(
                "SELECT hidden FROM pragma_table_xinfo(?1) WHERE name=?2",
                params![sheet.name, name],
                |row| row.get(0),
            )?;
            columns.push(ColumnMetadata {
                id: self.column_id(id, &name)?,
                name,
                declared_type: kind,
                writable: hidden == 0,
            });
        }
        let values = columns
            .iter()
            .map(|column| format!("t.{}", quote_identifier(&column.name)))
            .collect::<Vec<_>>()
            .join(",");
        let projection = sheet.locator.projection("t.");
        let normalized = ordering.trim();
        let asc = sheet.locator.alias.as_ref().is_some_and(|alias| {
            [
                format!("ORDER BY {alias}"),
                format!("ORDER BY {alias} ASC"),
                format!("ORDER BY {} ASC", quote_identifier(alias)),
            ]
            .contains(&normalized.to_owned())
        });
        let desc = sheet.locator.alias.as_ref().is_some_and(|alias| {
            [
                format!("ORDER BY {alias} DESC"),
                format!("ORDER BY {} DESC", quote_identifier(alias)),
            ]
            .contains(&normalized.to_owned())
        });
        let direct = normalized.is_empty() || asc || desc;
        let mut params = Vec::new();
        let query = if direct {
            let direction = if desc { "DESC" } else { "ASC" };
            let order = sheet
                .locator
                .columns
                .iter()
                .map(|name| format!("t.{} {direction}", quote_identifier(name)))
                .collect::<Vec<_>>()
                .join(",");
            let cursor = self
                .identities
                .borrow()
                .cursors
                .get(&(id, ordering.to_owned()))
                .cloned()
                .filter(|(next, _)| *next == offset);
            let tail = if let Some((_, keys)) = cursor {
                params = keys;
                let placeholders = (0..params.len()).map(|_| "?").collect::<Vec<_>>().join(",");
                format!(
                    "WHERE ({projection}) {} ({placeholders}) ORDER BY {order} LIMIT {limit}",
                    if desc { "<" } else { ">" }
                )
            } else {
                format!("ORDER BY {order} LIMIT {limit} OFFSET {offset}")
            };
            format!(
                "SELECT {values},{projection} FROM {} t {tail}",
                quote_identifier(&sheet.name)
            )
        } else {
            let map = format!("__dataman_session.order_{}", id.key);
            if self.identities.borrow().orders.get(&id).map(String::as_str) != Some(ordering) {
                let keys = (0..sheet.locator.columns.len())
                    .map(|i| format!("k{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let ties = sheet.locator.projection("");
                // Give locator fields private names regardless of source column names.
                self.identities.borrow_mut().orders.remove(&id);
                self.connection.execute_batch(&format!("DROP TABLE IF EXISTS {map}; CREATE TABLE {map}(position INTEGER PRIMARY KEY, {keys});"))?;
                self.connection.execute(&format!("INSERT INTO {map} SELECT ROW_NUMBER() OVER ({ordering},{ties})-1,{ties} FROM {};", quote_identifier(&sheet.name)), [])?;
                self.identities
                    .borrow_mut()
                    .orders
                    .insert(id, ordering.to_owned());
            }
            let join = sheet
                .locator
                .columns
                .iter()
                .enumerate()
                .map(|(i, name)| format!("t.{} IS p.k{i}", quote_identifier(name)))
                .collect::<Vec<_>>()
                .join(" AND ");
            format!("SELECT {values},{projection} FROM {map} p JOIN {} t ON {join} WHERE p.position>={offset} AND p.position<{} ORDER BY p.position", quote_identifier(&sheet.name), u64::from(offset)+limit)
        };
        let mut data = {
            let mut statement = self.connection.prepare(&query)?;
            let mut rows = statement.query(rusqlite::params_from_iter(params))?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                self.check_cancelled()?;
                let values = (0..columns.len())
                    .map(|i| row.get_ref(i).map(DataItem::from))
                    .collect::<Result<Vec<_>, _>>()?;
                let keys = (0..sheet.locator.columns.len())
                    .map(|i| row.get::<_, Value>(columns.len() + i))
                    .collect::<Result<Vec<_>, _>>()?;
                result.push((values, keys));
            }
            result
        };
        let has_more = data.len() as u64 > u64::from(requested_limit);
        data.truncate(requested_limit as usize);
        if requested_limit > 0 && (offset == 0 || !data.is_empty()) {
            let end = u64::from(offset) + data.len() as u64;
            let mut cache = self.cache.borrow_mut();
            let lower = cache.lower_bounds.entry(sheet.name.clone()).or_default();
            *lower = (*lower).max(end + u64::from(has_more));
            if !has_more && (offset == 0 || !data.is_empty()) {
                if let Ok(count) = u32::try_from(end) {
                    cache.counts.insert(sheet.name.clone(), count);
                }
            }
        }
        let row_ids = self.atomic_impl(
            || {
                data.iter()
                    .map(|(_, keys)| self.intern_row(&sheet, keys))
                    .collect()
            },
            false,
        )?;
        if direct {
            if let Some((_, keys)) = data.last() {
                self.identities.borrow_mut().cursors.insert(
                    (id, ordering.to_owned()),
                    (
                        offset
                            .checked_add(
                                u32::try_from(data.len())
                                    .map_err(|_| AppError::from("Unsupported page size"))?,
                            )
                            .ok_or_else(|| {
                                AppError::from("Row offset exceeds supported u32 range")
                            })?,
                        keys.clone(),
                    ),
                );
            }
        }
        Ok(Page {
            has_more,
            sheet: id,
            columns,
            row_ids,
            rows: data.into_iter().map(|(values, _)| values).collect(),
        })
    }

    pub fn cell_value(&self, id: SheetId, row: RowId, column: ColumnId) -> AppResult<DataItem> {
        self.source_read(|| self.cell_value_in_snapshot(id, row, column))
    }

    fn cell_value_in_snapshot(
        &self,
        id: SheetId,
        row: RowId,
        column: ColumnId,
    ) -> AppResult<DataItem> {
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, false)?;
        let name = self.column_name(id, column)?;
        let keys = self.row_locator(&sheet, row, true)?;
        Ok(self.connection.query_row(
            &format!(
                "SELECT {} FROM {} WHERE {}",
                quote_identifier(&name),
                quote_identifier(&sheet.name),
                sheet.locator.predicate("", 1)
            ),
            rusqlite::params_from_iter(keys),
            |row| row.get_ref(0).map(DataItem::from),
        )?)
    }

    pub(crate) fn edit_row_workspace(
        &self,
        id: SheetId,
        row: RowId,
        column: ColumnId,
        content: &str,
    ) -> AppResult<()> {
        self.edit_row_value(id, row, column, &DataItem::Text(content.to_owned()))
    }

    pub(super) fn edit_row_value(
        &self,
        id: SheetId,
        row: RowId,
        column: ColumnId,
        value: &DataItem,
    ) -> AppResult<()> {
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, true)?;
        let name = self.column_name(id, column)?;
        let owns_transaction = self.transaction_depth.get() == 0;
        self.atomic_values(&sheet.name, &name, || {
            let old = self.row_locator(&sheet, row, true)?;
            let params = std::iter::once(data_item_value(value))
                .chain(old)
                .collect::<Vec<_>>();
            let keys = self.connection.query_row(
                &format!(
                    "UPDATE {} SET {}=?1 WHERE {} RETURNING {}",
                    quote_identifier(&sheet.name),
                    quote_identifier(&name),
                    sheet.locator.predicate("", 2),
                    sheet.locator.projection("")
                ),
                rusqlite::params_from_iter(params),
                |record| {
                    (0..sheet.locator.columns.len())
                        .map(|i| record.get::<_, Value>(i))
                        .collect::<Result<Vec<_>, _>>()
                },
            )?;
            let exists = self.connection.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE {})",
                    quote_identifier(&sheet.name),
                    sheet.locator.predicate("", 1)
                ),
                rusqlite::params_from_iter(&keys),
                |record| record.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(AppError::from(
                    "A trigger relocated the edited row; the operation was rolled back",
                ));
            }
            self.row_locator(&sheet, row, true)?;
            let assignments = (0..keys.len())
                .map(|i| format!("k{i}=?{}", i + 1))
                .collect::<Vec<_>>()
                .join(",");
            let params = keys
                .into_iter()
                .chain([Value::Integer(row.key)])
                .collect::<Vec<_>>();
            self.connection.execute(
                &format!(
                    "UPDATE {} SET {assignments} WHERE handle=?{}",
                    map_name(id),
                    params.len()
                ),
                rusqlite::params_from_iter(params),
            )?;
            if owns_transaction {
                self.reconcile_if_triggered()?;
            }
            Ok(())
        })
    }

    fn selected_handles(&self, sheet: &Sheet, rows: &HashSet<RowId>) -> AppResult<()> {
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS __dataman_session.selected_handles(handle INTEGER PRIMARY KEY); DELETE FROM __dataman_session.selected_handles;")?;
        let mut insert = self
            .connection
            .prepare("INSERT INTO __dataman_session.selected_handles VALUES(?1)")?;
        for row in rows {
            self.check_cancelled()?;
            let keys = self.row_locator(sheet, *row, true)?;
            let exists = self.connection.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE {})",
                    quote_identifier(&sheet.name),
                    sheet.locator.predicate("", 1)
                ),
                rusqlite::params_from_iter(keys),
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(AppError::from("Selected row no longer exists"));
            }
            insert.execute([row.key])?;
        }
        Ok(())
    }

    fn selected_join(&self, sheet: &Sheet) -> String {
        let join = sheet
            .locator
            .columns
            .iter()
            .enumerate()
            .map(|(i, column)| format!("t.{} IS m.k{i}", quote_identifier(column)))
            .collect::<Vec<_>>()
            .join(" AND ");
        format!("{} t JOIN {} m ON {join} AND m.active=1 JOIN __dataman_session.selected_handles s ON s.handle=m.handle", quote_identifier(&sheet.name), map_name(sheet.id))
    }

    pub(crate) fn bulk_edit_rows_workspace(
        &self,
        id: SheetId,
        column: ColumnId,
        rows: &HashSet<RowId>,
        value: &str,
    ) -> AppResult<UndoRecord> {
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, true)?;
        let name = self.column_name(id, column)?;
        self.atomic(|| {
            self.selected_handles(&sheet, rows)?;
            let archive = self.internal_table_name()?;
            self.connection.execute(&format!("CREATE TABLE {} AS SELECT m.handle,t.{} AS before_value,?1 AS after_value FROM {}", quote_identifier(&archive), quote_identifier(&name), self.selected_join(&sheet)), [value])?;
            self.restore_row_bulk(id, column, &archive, false)?;
            Ok(UndoRecord::RowBulkEdit { sheet: id, column, archive, is_after: true })
        })
    }

    pub(super) fn restore_row_bulk(
        &self,
        id: SheetId,
        column: ColumnId,
        archive: &str,
        before: bool,
    ) -> AppResult<()> {
        let field = if before {
            "before_value"
        } else {
            "after_value"
        };
        let mut last = 0;
        loop {
            self.check_cancelled()?;
            let batch = {
                let mut statement = self.connection.prepare(&format!(
                    "SELECT handle,{field} FROM {} WHERE handle>?1 ORDER BY handle LIMIT 256",
                    quote_identifier(archive)
                ))?;
                let result = statement
                    .query_map([last], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get_ref(1).map(DataItem::from)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                result
            };
            if batch.is_empty() {
                break;
            }
            for (key, value) in batch {
                self.edit_row_value(
                    id,
                    RowId {
                        session: id.session,
                        key,
                    },
                    column,
                    &value,
                )?;
                last = key;
            }
        }
        self.reconcile_if_triggered()?;
        Ok(())
    }

    fn insertable_columns(&self, table: &str) -> AppResult<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden=0 ORDER BY cid")?;
        let columns = statement
            .query_map([table], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(columns)
    }

    pub(crate) fn delete_rows_workspace(
        &self,
        id: SheetId,
        rows: &HashSet<RowId>,
    ) -> AppResult<UndoRecord> {
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, true)?;
        self.atomic(|| {
            self.selected_handles(&sheet, rows)?;
            let archive = self.internal_table_name()?;
            let keys = sheet
                .locator
                .columns
                .iter()
                .enumerate()
                .map(|(i, column)| format!("t.{} AS k{i}", quote_identifier(column)))
                .collect::<Vec<_>>()
                .join(",");
            let columns = self
                .insertable_columns(&sheet.name)?
                .iter()
                .enumerate()
                .map(|(i, column)| format!("t.{} AS c{i}", quote_identifier(column)))
                .collect::<Vec<_>>()
                .join(",");
            self.connection.execute_batch(&format!(
                "CREATE TABLE {} AS SELECT m.handle,{keys},{columns} FROM {};",
                quote_identifier(&archive),
                self.selected_join(&sheet)
            ))?;
            self.restore_row_deletion(id, &archive, false)?;
            Ok(UndoRecord::RowDeleted {
                sheet: id,
                archive,
                is_deleted: true,
            })
        })
    }

    pub(super) fn restore_row_deletion(
        &self,
        id: SheetId,
        archive: &str,
        restore: bool,
    ) -> AppResult<()> {
        let sheet = self.sheet(id)?;
        let source = quote_identifier(&sheet.name);
        let archive = quote_identifier(archive);
        let map = map_name(id);
        if restore {
            let columns = self.insertable_columns(&sheet.name)?;
            let mut insert = columns
                .iter()
                .map(|column| quote_identifier(column))
                .collect::<Vec<_>>();
            let mut select = (0..columns.len())
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>();
            if sheet.locator.integer {
                insert.insert(0, quote_identifier(&sheet.locator.columns[0]));
                select.insert(0, "k0".into());
            }
            self.connection.execute_batch(&format!("INSERT INTO {source} ({}) SELECT {} FROM {archive}; UPDATE {map} SET active=1 WHERE handle IN (SELECT handle FROM {archive});", insert.join(","), select.join(",")))?;
        } else {
            let predicate = sheet
                .locator
                .columns
                .iter()
                .enumerate()
                .map(|(i, column)| format!("{source}.{} IS a.k{i}", quote_identifier(column)))
                .collect::<Vec<_>>()
                .join(" AND ");
            self.connection.execute_batch(&format!("DELETE FROM {source} WHERE EXISTS(SELECT 1 FROM {archive} a WHERE {predicate}); UPDATE {map} SET active=0 WHERE handle IN (SELECT handle FROM {archive});"))?;
            let remaining = self.connection.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {source} JOIN {archive} a ON {predicate})"),
                [],
                |row| row.get::<_, bool>(0),
            )?;
            if remaining {
                return Err(AppError::from(
                    "A trigger prevented deletion; the operation was rolled back",
                ));
            }
        }
        self.reconcile_if_triggered()?;
        Ok(())
    }

    pub(crate) fn copy_rows_workspace(
        &self,
        id: SheetId,
        target: &str,
        ordering: &str,
        rows: &HashSet<RowId>,
    ) -> AppResult<()> {
        let sheet = self.sheet(id)?;
        self.atomic(|| {
            self.selected_handles(&sheet, rows)?;
            let columns =
                super::super::metadata::visible_columns(&self.connection, &sheet.name, true)?
                    .iter()
                    .map(|(column, _)| format!("t.{}", quote_identifier(column)))
                    .collect::<Vec<_>>()
                    .join(",");
            let order = if ordering.is_empty() {
                format!("ORDER BY {}", sheet.locator.projection("t."))
            } else {
                format!("{ordering},{}", sheet.locator.projection("t."))
            };
            self.connection.execute_batch(&format!(
                "CREATE TABLE {} AS SELECT {columns} FROM {} {order};",
                quote_identifier(target),
                self.selected_join(&sheet)
            ))?;
            Ok(())
        })
    }

    pub fn select_where(&self, id: SheetId, condition: &str) -> AppResult<HashSet<RowId>> {
        self.source_read(|| self.select_where_in_snapshot(id, condition))
    }

    fn select_where_in_snapshot(&self, id: SheetId, condition: &str) -> AppResult<HashSet<RowId>> {
        let sheet = self.sheet(id)?;
        self.ensure_visible_sheet(&sheet, false)?;
        let projection = sheet.locator.projection("");
        self.atomic_impl(
            || {
                let mut statement = self.connection.prepare(&format!(
                    "SELECT {projection} FROM {} WHERE {condition}",
                    quote_identifier(&sheet.name)
                ))?;
                if !statement.readonly() {
                    return Err(AppError::from("Selection must be read-only"));
                }
                let mut rows = statement.query([])?;
                let mut result = HashSet::new();
                while let Some(row) = rows.next()? {
                    self.check_cancelled()?;
                    let keys = (0..sheet.locator.columns.len())
                        .map(|i| row.get::<_, Value>(i))
                        .collect::<Result<Vec<_>, _>>()?;
                    result.insert(self.intern_row(&sheet, &keys)?);
                }
                Ok(result)
            },
            false,
        )
    }

    pub(crate) fn select_regex_rows(
        &self,
        id: SheetId,
        column: ColumnId,
        pattern: &str,
    ) -> AppResult<HashSet<RowId>> {
        self.source_read(|| self.select_regex_rows_in_snapshot(id, column, pattern))
    }

    fn select_regex_rows_in_snapshot(
        &self,
        id: SheetId,
        column: ColumnId,
        pattern: &str,
    ) -> AppResult<HashSet<RowId>> {
        regex::Regex::new(pattern)?;
        let column = self.column_name(id, column)?;
        self.select_where(
            id,
            &format!(
                "regexp('{}', {})",
                pattern.replace('\'', "''"),
                quote_identifier(&column)
            ),
        )
    }

    pub(crate) fn rename_sheet_identity(&self, old: &str, new: &str) -> AppResult<()> {
        let previous = self.identities.borrow().sheets.get(new).cloned();
        let source = self.identities.borrow().sheets.get(old).cloned();
        if let Some(mut sheet) = source {
            let generated = sheet.id;
            if let Some(previous) = previous {
                // Refresh replaces storage, while the logical sheet survives.
                for trigger in &sheet.triggers {
                    self.connection.execute_batch(&format!(
                        "DROP TRIGGER IF EXISTS temp.{}",
                        quote_identifier(trigger)
                    ))?;
                }
                let definitions = sheet
                    .locator
                    .collations
                    .iter()
                    .enumerate()
                    .map(|(i, collation)| format!("k{i} COLLATE {}", quote_identifier(collation)))
                    .collect::<Vec<_>>()
                    .join(",");
                let keys = (0..sheet.locator.columns.len())
                    .map(|i| format!("k{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.connection.execute_batch(&format!("DROP TABLE {}; CREATE TABLE {}(handle INTEGER PRIMARY KEY,active INTEGER NOT NULL DEFAULT 1,{definitions}); CREATE UNIQUE INDEX __dataman_session.row_keys_{} ON rows_{} ({keys}) WHERE active=1;", map_name(previous.id), map_name(previous.id), previous.id.key, previous.id.key))?;
                let (tracker, triggers) =
                    self.install_identity_tracking(previous.id, new, &sheet.locator)?;
                sheet.id = previous.id;
                sheet.tracker = tracker;
                sheet.triggers = triggers;
                self.connection
                    .execute_batch(&format!("DROP TABLE {}", map_name(generated)))?;
            }
            sheet.name = new.to_owned();
            let mut registry = self.identities.borrow_mut();
            registry.sheets.remove(old);
            for (source, groups) in registry.memberships.values_mut() {
                if *source == generated {
                    *source = sheet.id;
                }
                if *groups == generated {
                    *groups = sheet.id;
                }
            }
            for snapshot in registry.snapshots.values_mut() {
                if snapshot == old {
                    *snapshot = new.to_owned();
                }
            }
            registry.sheets.insert(new.to_owned(), sheet);
        }
        Ok(())
    }

    pub(super) fn prepare_identity_column_rename(
        &self,
        table: &str,
        old: &str,
        new: &str,
    ) -> AppResult<()> {
        if super::super::metadata::is_owned(&self.connection, &format!("identity:{table}"), old)? {
            return Err(AppError::from("The private row locator cannot be renamed"));
        }
        let sheet = self.identities.borrow().sheets.get(table).cloned();
        if let Some(sheet) = sheet {
            if sheet.locator.integer && sheet.locator.columns[0].eq_ignore_ascii_case(new) {
                let mut statement = self
                    .connection
                    .prepare("SELECT name FROM pragma_table_xinfo(?1)")?;
                let names = statement
                    .query_map([table], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                let names = names
                    .into_iter()
                    .map(|name| {
                        if name.eq_ignore_ascii_case(old) {
                            new.to_owned()
                        } else {
                            name
                        }
                    })
                    .collect::<Vec<_>>();
                if ["rowid", "_rowid_", "oid"]
                    .iter()
                    .all(|alias| names.iter().any(|name| name.eq_ignore_ascii_case(alias)))
                {
                    return Err(AppError::from("This rename would hide the last SQLite row locator; use a snapshot before renaming it"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn rename_column_identity(
        &self,
        table: &str,
        old: &str,
        new: &str,
    ) -> AppResult<()> {
        let sheet = self.identities.borrow().sheets.get(table).cloned();
        if let Some(mut sheet) = sheet {
            let locator = self
                .discover_locator(table)?
                .ok_or_else(|| AppError::from("The rename removed this sheet's row locator"))?;
            for trigger in &sheet.triggers {
                self.connection.execute_batch(&format!(
                    "DROP TRIGGER IF EXISTS temp.{}",
                    quote_identifier(trigger)
                ))?;
            }
            let (tracker, triggers) = self.install_identity_tracking(sheet.id, table, &locator)?;
            sheet.locator = locator;
            sheet.tracker = tracker;
            sheet.triggers = triggers;
            let id = sheet.id;
            let mut registry = self.identities.borrow_mut();
            registry.sheets.insert(table.to_owned(), sheet);
            if let Some(column) = registry.columns.remove(&(id, old.to_owned())) {
                registry.columns.insert((id, new.to_owned()), column);
            }
        }
        Ok(())
    }

    pub(crate) fn reset_row_identities(&self, tables: &HashSet<String>) -> AppResult<()> {
        let sheets = self
            .identities
            .borrow()
            .sheets
            .values()
            .filter(|sheet| tables.contains(&sheet.name))
            .cloned()
            .collect::<Vec<_>>();
        for sheet in sheets {
            self.connection
                .execute(&format!("DELETE FROM {}", map_name(sheet.id)), [])?;
        }
        self.identities.borrow_mut().invalidate_orders();
        Ok(())
    }

    pub(super) fn reinstall_identity_tracking(&self) -> AppResult<()> {
        let sheets = self
            .identities
            .borrow()
            .sheets
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sheet in sheets {
            let (tracker, triggers) =
                self.install_identity_tracking(sheet.id, &sheet.name, &sheet.locator)?;
            if let Some(current) = self.identities.borrow_mut().sheets.get_mut(&sheet.name) {
                current.tracker = tracker;
                current.triggers = triggers;
            }
        }
        Ok(())
    }

    fn install_identity_tracking(
        &self,
        id: SheetId,
        table: &str,
        locator: &Locator,
    ) -> AppResult<(String, Vec<String>)> {
        if self.storage_state == StorageState::ReadOnlySource {
            return Ok((String::new(), Vec::new()));
        }
        let tracker = format!("__dataman_track_{}_{}", id.session, id.key);
        let keys = locator.clone();
        let map = map_name(id);
        let enabled = self.identities.borrow().tracking_enabled.clone();
        self.connection.create_scalar_function(
            &tracker,
            (1 + 2 * locator.columns.len()) as i32,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |context| {
                if !enabled.load(Ordering::Relaxed) {
                    return Ok(0i64);
                }
                let kind = context.get::<i64>(0)?;
                let old = (0..keys.columns.len())
                    .map(|i| context.get::<Value>(1 + i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let new = (0..keys.columns.len())
                    .map(|i| context.get::<Value>(1 + keys.columns.len() + i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                // SAFETY: Database's Connection is !Sync and is exclusively owned by
                // one worker. This borrowed handle never escapes this synchronous
                // callback. Re-entrant statements touch only the session sidecar.
                let connection = unsafe { context.get_connection()? };
                if kind == 0 {
                    connection.execute(
                        &format!(
                            "UPDATE {map} SET active=0 WHERE active=1 AND {}",
                            keys.map_predicate(1)
                        ),
                        rusqlite::params_from_iter(new),
                    )?;
                } else if kind == 1 {
                    connection.execute(
                        &format!(
                            "UPDATE {map} SET active=0 WHERE active=1 AND {}",
                            keys.map_predicate(1)
                        ),
                        rusqlite::params_from_iter(old),
                    )?;
                } else {
                    let handle = connection
                        .query_row(
                            &format!(
                                "SELECT handle FROM {map} WHERE active=1 AND {}",
                                keys.map_predicate(1)
                            ),
                            rusqlite::params_from_iter(&old),
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?;
                    // UPDATE OR REPLACE may delete an already-observed destination.
                    let mut params = new.clone();
                    params.push(Value::Integer(handle.unwrap_or(-1)));
                    connection.execute(
                        &format!(
                            "UPDATE {map} SET active=0 WHERE active=1 AND {} AND handle!=?{}",
                            keys.map_predicate(1),
                            params.len()
                        ),
                        rusqlite::params_from_iter(params),
                    )?;
                    if let Some(handle) = handle {
                        let assignments = (0..new.len())
                            .map(|i| format!("k{i}=?{}", i + 1))
                            .collect::<Vec<_>>()
                            .join(",");
                        let mut params = new;
                        params.push(Value::Integer(handle));
                        connection.execute(
                            &format!(
                                "UPDATE {map} SET {assignments} WHERE handle=?{}",
                                params.len()
                            ),
                            rusqlite::params_from_iter(params),
                        )?;
                    }
                }
                Ok(0i64)
            },
        )?;
        let schema = if self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
            [table],
            |row| row.get::<_, bool>(0),
        )? {
            "main"
        } else {
            "temp"
        };
        let old = locator
            .columns
            .iter()
            .map(|key| format!("OLD.{}", quote_identifier(key)))
            .collect::<Vec<_>>()
            .join(",");
        let new = locator
            .columns
            .iter()
            .map(|key| format!("NEW.{}", quote_identifier(key)))
            .collect::<Vec<_>>()
            .join(",");
        let nulls = (0..locator.columns.len())
            .map(|_| "NULL")
            .collect::<Vec<_>>()
            .join(",");
        let mut triggers = Vec::new();
        for (event, args) in [
            ("INSERT", format!("0,{nulls},{new}")),
            ("DELETE", format!("1,{old},{nulls}")),
            ("UPDATE", format!("2,{old},{new}")),
        ] {
            let mut trigger = format!("{tracker}_{event}");
            while self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name=?1)",
                [&trigger],
                |row| row.get::<_, bool>(0),
            )? {
                trigger.push('_');
            }
            self.connection.execute_batch(&format!(
                "CREATE TEMP TRIGGER {} AFTER {event} ON {schema}.{} BEGIN SELECT {}({args}); END;",
                quote_identifier(&trigger),
                quote_identifier(table),
                quote_identifier(&tracker)
            ))?;
            triggers.push(trigger);
        }
        Ok((tracker, triggers))
    }

    pub(super) fn reconcile_row_handles(&self) -> AppResult<()> {
        let sheets = self
            .identities
            .borrow()
            .sheets
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sheet in sheets {
            let map = map_name(sheet.id);
            let predicate = sheet
                .locator
                .columns
                .iter()
                .enumerate()
                .map(|(i, column)| format!("t.{} IS {map}.k{i}", quote_identifier(column)))
                .collect::<Vec<_>>()
                .join(" AND ");
            self.connection.execute(&format!("UPDATE {map} SET active=0 WHERE active=1 AND NOT EXISTS(SELECT 1 FROM {} t WHERE {predicate})", quote_identifier(&sheet.name)), [])?;
        }
        Ok(())
    }

    pub(super) fn reconcile_if_triggered(&self) -> AppResult<()> {
        if self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger')",
            [],
            |row| row.get::<_, bool>(0),
        )? {
            self.reconcile_row_handles()?;
        }
        Ok(())
    }

    pub(super) fn suspend_identity_tracking(&self) -> TrackingGuard {
        let enabled = self.identities.borrow().tracking_enabled.clone();
        enabled.store(false, Ordering::Relaxed);
        TrackingGuard(enabled)
    }

    pub(super) fn identity_trackers(&self) -> (HashSet<String>, HashSet<String>) {
        let registry = self.identities.borrow();
        (
            registry
                .sheets
                .values()
                .map(|sheet| sheet.tracker.clone())
                .collect(),
            registry
                .sheets
                .values()
                .flat_map(|sheet| sheet.triggers.iter().cloned())
                .collect(),
        )
    }

    pub(super) fn identity_functions(&self) -> Vec<(String, i32)> {
        self.identities
            .borrow()
            .sheets
            .values()
            .map(|sheet| {
                (
                    sheet.tracker.clone(),
                    (1 + 2 * sheet.locator.columns.len()) as i32,
                )
            })
            .collect()
    }

    pub(super) fn retire_identity_functions(&self, functions: Vec<(String, i32)>) {
        let active = self.identity_trackers().0;
        for (name, arity) in functions {
            if !active.contains(&name) {
                if let Err(error) = self.connection.remove_function(&name, arity) {
                    log::warn!("Could not release an obsolete identity tracker: {error}");
                }
            }
        }
    }

    pub(super) fn forget_column_identity(&self, table: &str, column: &str) {
        let mut registry = self.identities.borrow_mut();
        if let Some(id) = registry.sheets.get(table).map(|sheet| sheet.id) {
            registry.columns.remove(&(id, column.to_owned()));
        }
    }

    pub(super) fn forget_sheet_identity(&self, table: &str) -> AppResult<()> {
        let indices = {
            let mut statement = self
                .connection
                .prepare("SELECT name FROM __dataman_session.owned_objects WHERE scope=?1")?;
            let names = statement
                .query_map([format!("index:{table}")], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            names
        };
        for index in indices {
            super::super::metadata::disown(&self.connection, "index", &index)?;
        }
        for kind in ["column", "export", "identity", "index"] {
            self.connection.execute(
                "DELETE FROM __dataman_session.owned_objects WHERE scope=?1",
                [format!("{kind}:{table}")],
            )?;
        }
        let sheet = self.identities.borrow_mut().sheets.remove(table);
        if let Some(sheet) = sheet {
            for trigger in sheet.triggers {
                self.connection.execute_batch(&format!(
                    "DROP TRIGGER IF EXISTS temp.{}",
                    quote_identifier(&trigger)
                ))?;
            }
            self.connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS __dataman_session.order_{};",
                map_name(sheet.id),
                sheet.id.key
            ))?;
            self.identities
                .borrow_mut()
                .columns
                .retain(|(owner, _), _| *owner != sheet.id);
        }
        Ok(())
    }

    pub(crate) fn register_frequency(
        &self,
        members: &str,
        source: &str,
        groups: &str,
    ) -> AppResult<()> {
        let source = self.open_sheet_existing(source)?;
        let groups = self.open_sheet_existing(groups)?;
        self.identities
            .borrow_mut()
            .memberships
            .insert(members.to_owned(), (source, groups));
        Ok(())
    }

    pub(crate) fn frequency_members(
        &self,
        membership: &str,
        group: RowId,
    ) -> AppResult<HashSet<RowId>> {
        let (source, groups) = self
            .identities
            .borrow()
            .memberships
            .get(membership)
            .copied()
            .ok_or_else(|| AppError::from("Frequency provenance is stale"))?;
        let group_column = self.column_id(groups, "id")?;
        let DataItem::Integer(group_key) = self.cell_value(groups, group, group_column)? else {
            return Err(AppError::from("Frequency key is invalid"));
        };
        let sheet = self.sheet(source)?;
        let keys = (0..sheet.locator.columns.len())
            .map(|i| format!("k{i}"))
            .collect::<Vec<_>>()
            .join(",");
        self.atomic_impl(|| {
            let mut result = HashSet::new();
            let mut last = 0;
            loop {
                self.check_cancelled()?;
                let data = {
                    let mut statement = self.connection.prepare(&format!("SELECT rowid,{keys} FROM {} WHERE group_id=?1 AND rowid>?2 ORDER BY rowid LIMIT 256", quote_identifier(membership)))?;
                    let data = statement.query_map(params![group_key, last], |row| Ok((row.get::<_, i64>(0)?, (0..sheet.locator.columns.len()).map(|i| row.get::<_, Value>(i+1)).collect::<Result<Vec<_>, _>>()?)))?.collect::<Result<Vec<_>, _>>()?;
                    data
                };
                if data.is_empty() { break; }
                for (position, keys) in data { result.insert(self.intern_row(&sheet, &keys)?); last = position; }
            }
            Ok(result)
        }, false)
    }

    pub(super) fn frequency_locator_sql(
        &self,
        source: &str,
    ) -> AppResult<(String, String, String)> {
        let sheet = self.sheet(self.open_sheet_existing(source)?)?;
        let definitions = (0..sheet.locator.columns.len())
            .map(|i| format!("k{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let projection = sheet.locator.projection("source.");
        let first = format!("source.{}", quote_identifier(&sheet.locator.columns[0]));
        Ok((definitions, projection, first))
    }

    pub(super) fn restore_registry(&self, mut previous: IdentityRegistry) {
        previous.next = previous.next.max(self.identities.borrow().next);
        *self.identities.borrow_mut() = previous;
    }

    pub(super) fn discard_deleted_handles(&self, id: SheetId, archive: &str) -> AppResult<()> {
        if self.sheet(id).is_ok()
            && self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [archive],
                |row| row.get::<_, bool>(0),
            )?
        {
            self.connection.execute(
                &format!(
                    "DELETE FROM {} WHERE active=0 AND handle IN (SELECT handle FROM {})",
                    map_name(id),
                    quote_identifier(archive)
                ),
                [],
            )?;
        }
        Ok(())
    }

    pub(crate) fn sql_changed_sheets(&self) -> HashSet<String> {
        self.identities.borrow().last_sql_changes.clone()
    }

    pub(super) fn finish_sql_identity_boundary(
        &self,
        mut changed: HashSet<String>,
        schema_changed: bool,
    ) -> AppResult<()> {
        if schema_changed {
            changed.extend(self.identities.borrow().sheets.keys().cloned());
        }
        self.reset_row_identities(&changed)?;
        if schema_changed {
            let sheets = self
                .identities
                .borrow()
                .sheets
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for sheet in sheets {
                for trigger in sheet.triggers {
                    self.connection.execute_batch(&format!(
                        "DROP TRIGGER IF EXISTS temp.{}",
                        quote_identifier(&trigger)
                    ))?;
                }
                self.connection.execute_batch(&format!(
                    "DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS __dataman_session.order_{};",
                    map_name(sheet.id),
                    sheet.id.key
                ))?;
            }
        }
        let mut registry = self.identities.borrow_mut();
        if schema_changed {
            registry.sheets.clear();
            registry.columns.clear();
            registry.memberships.clear();
            registry.snapshots.clear();
        }
        registry.last_sql_changes = changed;
        Ok(())
    }

    pub(crate) fn search_rows(
        &self,
        id: SheetId,
        ordering: &str,
        current: u32,
        column: ColumnId,
        pattern: &str,
    ) -> AppResult<Option<u32>> {
        self.source_read(|| self.search_rows_in_snapshot(id, ordering, current, column, pattern))
    }

    fn search_rows_in_snapshot(
        &self,
        id: SheetId,
        ordering: &str,
        current: u32,
        column: ColumnId,
        pattern: &str,
    ) -> AppResult<Option<u32>> {
        self.check_cancelled()?;
        let sheet = self.sheet(id)?;
        let column = self.column_name(id, column)?;
        let order = if ordering.is_empty() {
            format!("ORDER BY {}", sheet.locator.projection(""))
        } else {
            format!("{ordering},{}", sheet.locator.projection(""))
        };
        Ok(self.connection.query_row(&format!("SELECT position FROM (SELECT ROW_NUMBER() OVER ({order})-1 AS position,{} AS value FROM {}) WHERE position>?1 AND value=?2 ORDER BY position LIMIT 1", quote_identifier(&column), quote_identifier(&sheet.name)), params![current, pattern], |row| row.get(0)).optional()?)
    }

    pub(crate) fn row_position(&self, id: SheetId, row: RowId, ordering: &str) -> AppResult<u32> {
        self.source_read(|| self.row_position_in_snapshot(id, row, ordering))
    }

    fn row_position_in_snapshot(&self, id: SheetId, row: RowId, ordering: &str) -> AppResult<u32> {
        self.check_cancelled()?;
        let sheet = self.sheet(id)?;
        let keys = self.row_locator(&sheet, row, true)?;
        let order = if ordering.is_empty() {
            format!("ORDER BY {}", sheet.locator.projection(""))
        } else {
            format!("{ordering},{}", sheet.locator.projection(""))
        };
        let projection = sheet
            .locator
            .columns
            .iter()
            .enumerate()
            .map(|(i, column)| format!("{} AS k{i}", quote_identifier(column)))
            .collect::<Vec<_>>()
            .join(",");
        Ok(self.connection.query_row(&format!("SELECT position FROM (SELECT ROW_NUMBER() OVER ({order})-1 AS position,{projection} FROM {}) WHERE {}", quote_identifier(&sheet.name), sheet.locator.map_predicate(1)), rusqlite::params_from_iter(keys), |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(sql: &str) -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(sql).unwrap();
        Database::new(connection).unwrap()
    }

    #[test]
    fn duplicate_user_ids_and_column_reordering_do_not_identify_rows() {
        let mut db = database("CREATE TABLE t(id TEXT, value TEXT); INSERT INTO t VALUES('001','same'),('001','same'),(NULL,'other');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        assert_ne!(page.row_ids[0], page.row_ids[1]);
        let column = db.column_id(sheet, "value").unwrap();
        db.edit_row(sheet, page.row_ids[1], column, "edited")
            .unwrap();
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("same".into())
        );
        assert_eq!(
            db.cell_value(sheet, page.row_ids[1], column).unwrap(),
            DataItem::Text("edited".into())
        );
        let sorted = db.fetch_page(sheet, 10, 0, "ORDER BY value DESC").unwrap();
        assert!(sorted.row_ids.contains(&page.row_ids[1]));
    }

    #[test]
    fn composite_without_rowid_keys_can_change_without_changing_the_handle() {
        let mut db = database("CREATE TABLE t(a TEXT COLLATE NOCASE, b TEXT, value TEXT, PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO t VALUES('A','x','first'),('B','x','second');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 1, 0, "").unwrap();
        let row = page.row_ids[0];
        let column = db.column_id(sheet, "a").unwrap();
        db.edit_row(sheet, row, column, "C").unwrap();
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("C".into())
        );
        assert!(db.edit_row(sheet, row, column, "b").is_err());
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("C".into())
        );
        let selected = db.select_where(sheet, "a='c'").unwrap();
        assert_eq!(selected, HashSet::from([row]));
        let first = db.fetch_page(sheet, 1, 0, "").unwrap();
        let next = db.fetch_page(sheet, 1, 1, "").unwrap();
        assert_ne!(first.row_ids, next.row_ids);
        assert_eq!(next.row_ids[0], row);
    }

    #[test]
    fn signed_storage_rowids_survive_deletion_and_undo() {
        let mut db = database("CREATE TABLE t(value TEXT); INSERT INTO t(rowid,value) VALUES(-9223372036854775808,'min'),(9223372036854775807,'max');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 2, 0, "").unwrap();
        let column = db.column_id(sheet, "value").unwrap();
        let mut record = db
            .delete_rows(sheet, &HashSet::from([page.row_ids[0]]))
            .unwrap();
        assert!(db.cell_value(sheet, page.row_ids[0], column).is_err());
        db.toggle_history_record(&mut record).unwrap();
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("min".into())
        );
        assert_eq!(
            db.fetch_page(sheet, 2, 0, "").unwrap().row_ids,
            page.row_ids
        );
        db.toggle_history_record(&mut record).unwrap();
        assert_eq!(db.count_rows("t").unwrap(), 1);
    }

    #[test]
    fn bulk_constraint_failure_rolls_back_values_and_identity_relocations() {
        let mut db = database("CREATE TABLE t(id TEXT PRIMARY KEY, value TEXT) WITHOUT ROWID; INSERT INTO t VALUES('a','first'),('b','second');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        let column = db.column_id(sheet, "id").unwrap();
        assert!(db
            .bulk_edit_rows(
                sheet,
                column,
                &page.row_ids.iter().copied().collect(),
                "same"
            )
            .is_err());
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("a".into())
        );
        assert_eq!(
            db.cell_value(sheet, page.row_ids[1], column).unwrap(),
            DataItem::Text("b".into())
        );
    }

    #[test]
    fn bulk_undo_and_redo_preserve_native_null_blob_and_numeric_values() {
        let mut db =
            database("CREATE TABLE t(value); INSERT INTO t VALUES(NULL),(X'00FF'),(1),(1.5);");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        let column = db.column_id(sheet, "value").unwrap();
        let mut record = db
            .bulk_edit_rows(
                sheet,
                column,
                &page.row_ids.iter().copied().collect(),
                "changed",
            )
            .unwrap();
        db.toggle_history_record(&mut record).unwrap();
        assert_eq!(db.fetch_page(sheet, 10, 0, "").unwrap().rows, page.rows);
        db.toggle_history_record(&mut record).unwrap();
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("changed".into())
        );
    }

    #[test]
    fn handles_survive_table_and_column_rename_and_reject_other_sheets_and_sessions() {
        let mut db = database("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO t VALUES(1,'x'); CREATE TABLE other(value TEXT); INSERT INTO other VALUES('y');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        let column = db.column_id(sheet, "id").unwrap();
        db.rename_column("t", "id", "key").unwrap();
        db.rename_table("t", "renamed").unwrap();
        assert_eq!(db.open_sheet("renamed").unwrap(), sheet);
        assert_eq!(db.column_id(sheet, "key").unwrap(), column);
        db.edit_row(sheet, page.row_ids[0], column, "4").unwrap();
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Integer(4)
        );
        let other = db.open_sheet("other").unwrap();
        assert!(db
            .cell_value(
                other,
                page.row_ids[0],
                db.column_id(other, "value").unwrap()
            )
            .is_err());
        let mut second = database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('another');");
        let second_sheet = second.open_sheet("t").unwrap();
        assert!(second
            .cell_value(
                second_sheet,
                page.row_ids[0],
                second.column_id(second_sheet, "value").unwrap()
            )
            .is_err());
    }

    #[test]
    fn view_and_fully_shadowed_rowid_sources_open_editable_private_snapshots() {
        let mut db = database("CREATE TABLE t(rowid TEXT, _rowid_ TEXT, oid TEXT, id TEXT); INSERT INTO t VALUES('a','b','c','001'); CREATE VIEW v AS SELECT id FROM t;");
        for source in ["t", "v"] {
            let sheet = db.open_sheet(source).unwrap();
            assert_ne!(db.sheet_name(sheet).unwrap(), source);
            let page = db.fetch_page(sheet, 10, 0, "").unwrap();
            let column = db.column_id(sheet, "id").unwrap();
            db.edit_row(sheet, page.row_ids[0], column, "copy").unwrap();
            assert_eq!(
                db.cell_value(sheet, page.row_ids[0], column).unwrap(),
                DataItem::Text("copy".into())
            );
        }
        assert_eq!(
            db.connection
                .query_row("SELECT id FROM t", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "001"
        );
    }

    #[test]
    fn sql_mutations_invalidate_only_affected_sheet_rows_after_commit() {
        let mut db = database("CREATE TABLE a(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO a VALUES(1,'a'); CREATE TABLE b(value TEXT); INSERT INTO b VALUES('b');");
        let a = db.open_sheet("a").unwrap();
        let b = db.open_sheet("b").unwrap();
        let old_a = db.fetch_page(a, 10, 0, "").unwrap().row_ids[0];
        let old_b = db.fetch_page(b, 10, 0, "").unwrap().row_ids[0];
        let ac = db.column_id(a, "value").unwrap();
        let bc = db.column_id(b, "value").unwrap();
        assert!(db
            .sql_query("UPDATE a SET value='bad'; INSERT INTO missing VALUES(1)")
            .is_err());
        assert_eq!(
            db.cell_value(a, old_a, ac).unwrap(),
            DataItem::Text("a".into())
        );
        db.sql_query("DELETE FROM a; INSERT INTO a VALUES(1,'replacement')")
            .unwrap();
        assert!(db.cell_value(a, old_a, ac).is_err());
        assert_eq!(
            db.cell_value(b, old_b, bc).unwrap(),
            DataItem::Text("b".into())
        );
        assert_ne!(db.fetch_page(a, 10, 0, "").unwrap().row_ids[0], old_a);
        db.sql_query("DROP TABLE a").unwrap();
    }

    #[test]
    fn first_page_interns_only_visible_rows_and_does_not_build_an_order_map() {
        let mut db = database("CREATE TABLE t(value TEXT); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO t SELECT 'x' FROM n;");
        let sheet = db.open_sheet("t").unwrap();
        db.fetch_page(sheet, 100, 0, "").unwrap();
        assert_eq!(
            db.connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {}", map_name(sheet)),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            100
        );
        assert!(db.identities.borrow().orders.is_empty());
        db.fetch_page(sheet, 100, 100, "").unwrap();
        assert_eq!(
            db.connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {}", map_name(sheet)),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            200
        );
    }

    #[test]
    fn cancellation_preserves_values_and_existing_handles() {
        let mut db = database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('original');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "").unwrap();
        let column = db.column_id(sheet, "value").unwrap();
        db.cancellation.store(true, Ordering::Relaxed);
        assert!(db
            .edit_row(sheet, page.row_ids[0], column, "cancelled")
            .unwrap_err()
            .is_cancelled());
        db.cancellation.store(false, Ordering::Relaxed);
        assert_eq!(
            db.cell_value(sheet, page.row_ids[0], column).unwrap(),
            DataItem::Text("original".into())
        );
    }

    #[test]
    fn trigger_key_updates_preserve_other_observed_row_handles() {
        let mut db = database("CREATE TABLE a(value TEXT); INSERT INTO a VALUES('start'); CREATE TABLE b(k TEXT PRIMARY KEY, value TEXT) WITHOUT ROWID; INSERT INTO b VALUES('before','payload'); CREATE TRIGGER change_key AFTER UPDATE ON a BEGIN UPDATE b SET k='after' WHERE k='before'; END;");
        let a = db.open_sheet("a").unwrap();
        let b = db.open_sheet("b").unwrap();
        let ar = db.fetch_page(a, 1, 0, "").unwrap().row_ids[0];
        let br = db.fetch_page(b, 1, 0, "").unwrap().row_ids[0];
        db.edit_row(a, ar, db.column_id(a, "value").unwrap(), "trigger")
            .unwrap();
        assert_eq!(
            db.cell_value(b, br, db.column_id(b, "k").unwrap()).unwrap(),
            DataItem::Text("after".into())
        );
    }

    #[test]
    fn trigger_delete_and_reinsert_does_not_reuse_the_deleted_handle() {
        let mut db = database("CREATE TABLE a(value TEXT); INSERT INTO a VALUES('start'); CREATE TABLE b(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO b VALUES(1,'old'); CREATE TRIGGER replace_row AFTER UPDATE ON a BEGIN DELETE FROM b WHERE id=1; INSERT INTO b VALUES(1,'replacement'); END;");
        let a = db.open_sheet("a").unwrap();
        let b = db.open_sheet("b").unwrap();
        let ar = db.fetch_page(a, 1, 0, "").unwrap().row_ids[0];
        let br = db.fetch_page(b, 1, 0, "").unwrap().row_ids[0];
        db.edit_row(a, ar, db.column_id(a, "value").unwrap(), "trigger")
            .unwrap();
        assert!(db
            .cell_value(b, br, db.column_id(b, "value").unwrap())
            .is_err());
        assert_ne!(db.fetch_page(b, 1, 0, "").unwrap().row_ids[0], br);
    }

    #[test]
    fn trigger_replace_invalidates_the_replaced_destination_handle() {
        let mut db = database("CREATE TABLE a(value TEXT); INSERT INTO a VALUES('start'); CREATE TABLE b(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO b VALUES(1,'first'),(2,'second'); CREATE TRIGGER replace_row AFTER UPDATE ON a BEGIN UPDATE OR REPLACE b SET id=2 WHERE id=1; END;");
        let a = db.open_sheet("a").unwrap();
        let b = db.open_sheet("b").unwrap();
        let ar = db.fetch_page(a, 1, 0, "").unwrap().row_ids[0];
        let page = db.fetch_page(b, 2, 0, "").unwrap();
        db.edit_row(a, ar, db.column_id(a, "value").unwrap(), "trigger")
            .unwrap();
        let column = db.column_id(b, "value").unwrap();
        assert_eq!(
            db.cell_value(b, page.row_ids[0], column).unwrap(),
            DataItem::Text("first".into())
        );
        assert!(db.cell_value(b, page.row_ids[1], column).is_err());
    }

    #[test]
    fn ignored_deletions_roll_back_without_tombstoning_the_row() {
        let mut db = database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('keep'); CREATE TRIGGER keep_row BEFORE DELETE ON t BEGIN SELECT RAISE(IGNORE); END;");
        let sheet = db.open_sheet("t").unwrap();
        let row = db.fetch_page(sheet, 1, 0, "").unwrap().row_ids[0];
        assert!(db.delete_rows(sheet, &HashSet::from([row])).is_err());
        assert_eq!(
            db.cell_value(sheet, row, db.column_id(sheet, "value").unwrap())
                .unwrap(),
            DataItem::Text("keep".into())
        );
    }

    #[test]
    fn key_moving_after_triggers_fail_atomically_when_returning_is_obsolete() {
        let mut db = database("CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO t VALUES(1,'keep'); CREATE TRIGGER move_row AFTER UPDATE OF value ON t BEGIN UPDATE t SET id=id+10 WHERE id=NEW.id; END;");
        let sheet = db.open_sheet("t").unwrap();
        let row = db.fetch_page(sheet, 1, 0, "").unwrap().row_ids[0];
        let column = db.column_id(sheet, "value").unwrap();
        assert!(db.edit_row(sheet, row, column, "move").is_err());
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("keep".into())
        );
    }

    #[test]
    fn renaming_a_user_column_to_rowid_uses_another_native_alias() {
        let mut db =
            database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('first'),('second');");
        let sheet = db.open_sheet("t").unwrap();
        let rows = db.fetch_page(sheet, 10, 0, "").unwrap().row_ids;
        let column = db.column_id(sheet, "value").unwrap();
        db.rename_column("t", "value", "rowid").unwrap();
        assert_eq!(db.column_id(sheet, "ROWID").unwrap(), column);
        db.edit_row(sheet, rows[1], column, "changed").unwrap();
        assert_eq!(
            db.cell_value(sheet, rows[0], column).unwrap(),
            DataItem::Text("first".into())
        );
        assert_eq!(
            db.cell_value(sheet, rows[1], column).unwrap(),
            DataItem::Text("changed".into())
        );
    }

    #[test]
    fn renames_that_remove_the_last_locator_fail_without_changing_data() {
        let mut db = database("CREATE TABLE t(rowid TEXT,_rowid_ TEXT,value TEXT); INSERT INTO t VALUES('a','b','keep');");
        let sheet = db.open_sheet("t").unwrap();
        let row = db.fetch_page(sheet, 1, 0, "").unwrap().row_ids[0];
        let column = db.column_id(sheet, "value").unwrap();
        assert!(db.rename_column("t", "value", "oid").is_err());
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("keep".into())
        );
    }

    #[test]
    fn foreign_key_cascades_preserve_composite_child_row_handles() {
        let mut db = database("CREATE TABLE parent(id INTEGER PRIMARY KEY); INSERT INTO parent VALUES(1); CREATE TABLE child(parent INTEGER REFERENCES parent(id) ON UPDATE CASCADE, key TEXT, value TEXT, PRIMARY KEY(parent,key)) WITHOUT ROWID; INSERT INTO child VALUES(1,'a','keep');");
        let parent = db.open_sheet("parent").unwrap();
        let child = db.open_sheet("child").unwrap();
        let pr = db.fetch_page(parent, 1, 0, "").unwrap().row_ids[0];
        let cr = db.fetch_page(child, 1, 0, "").unwrap().row_ids[0];
        db.edit_row(parent, pr, db.column_id(parent, "id").unwrap(), "2")
            .unwrap();
        assert_eq!(
            db.cell_value(child, cr, db.column_id(child, "parent").unwrap())
                .unwrap(),
            DataItem::Integer(2)
        );
    }

    #[test]
    fn sorted_positions_are_rebuilt_after_value_edits() {
        let mut db = database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('b'),('a');");
        let sheet = db.open_sheet("t").unwrap();
        let page = db.fetch_page(sheet, 10, 0, "ORDER BY value ASC").unwrap();
        let row = page.row_ids[1];
        db.edit_row(sheet, row, db.column_id(sheet, "value").unwrap(), "0")
            .unwrap();
        assert_eq!(
            db.fetch_page(sheet, 10, 0, "ORDER BY value ASC")
                .unwrap()
                .row_ids[0],
            row
        );
    }

    #[test]
    fn frequency_membership_supports_quoted_composite_keys_and_no_id_column() {
        let mut db = database("CREATE TABLE t(\"a,b\" TEXT, \"q\"\"uote\" TEXT, value TEXT, PRIMARY KEY(\"a,b\",\"q\"\"uote\")) WITHOUT ROWID; INSERT INTO t VALUES('a','x','same'),('b','x','same');");
        let source = db.open_sheet("t").unwrap();
        let (name, membership) = db.frequency_table_with_members("t", "value").unwrap();
        let groups = db.open_sheet(&name).unwrap();
        let group = db.fetch_page(groups, 1, 0, "").unwrap().row_ids[0];
        let members = db.frequency_members(&membership, group).unwrap();
        assert_eq!(members, db.select_where(source, "value='same'").unwrap());
    }

    #[test]
    fn schema_sql_resets_handles_and_cannot_call_private_trackers() {
        let mut db = database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('keep');");
        let sheet = db.open_sheet("t").unwrap();
        let row = db.fetch_page(sheet, 1, 0, "").unwrap().row_ids[0];
        let column = db.column_id(sheet, "value").unwrap();
        let tracker = db.identities.borrow().sheets["t"].tracker.clone();
        assert!(db.sql_query(&format!("SELECT {tracker}(2,1,2)")).is_err());
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("keep".into())
        );
        db.sql_query("ALTER TABLE t RENAME COLUMN value TO renamed")
            .unwrap();
        assert!(db.cell_value(sheet, row, column).is_err());
        let fresh = db.open_sheet("t").unwrap();
        assert_eq!(
            db.fetch_page(fresh, 1, 0, "").unwrap().columns[0].name,
            "renamed"
        );
    }

    #[test]
    fn cancellation_during_an_actual_bulk_update_rolls_back_values_and_handles() {
        let mut db = database("CREATE TABLE t(value TEXT); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO t SELECT 'original' FROM n;");
        let sheet = db.open_sheet("t").unwrap();
        let rows = db.select_where(sheet, "1").unwrap();
        let row = *rows.iter().next().unwrap();
        let column = db.column_id(sheet, "value").unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let observed = changed.clone();
        db.connection
            .update_hook(Some(move |action, database: &str, table: &str, _| {
                if action == rusqlite::hooks::Action::SQLITE_UPDATE
                    && database == "main"
                    && table == "t"
                {
                    observed.store(true, Ordering::Relaxed);
                }
            }));
        let token = db.cancellation.clone();
        let observed = changed.clone();
        db.connection.progress_handler(
            1,
            Some(move || {
                if observed.load(Ordering::Relaxed) {
                    token.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }),
        );
        let error = db
            .bulk_edit_rows(sheet, column, &rows, "cancelled")
            .unwrap_err();
        assert!(changed.load(Ordering::Relaxed));
        assert!(error.is_cancelled(), "{error}");
        db.cancellation.store(false, Ordering::Relaxed);
        assert_eq!(
            db.cell_value(sheet, row, column).unwrap(),
            DataItem::Text("original".into())
        );
        assert_eq!(
            db.connection
                .query_row(
                    "SELECT COUNT(*) FROM t WHERE value!='original'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn expired_deleted_sheets_cannot_resurrect_old_sheet_ids() {
        let mut db=database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('old'); CREATE TABLE other(value TEXT); INSERT INTO other VALUES('new');");
        let old = db.open_sheet("t").unwrap();
        let other = db.open_sheet("other").unwrap();
        let hidden = db.delete_table("t").unwrap();
        db.discard_hidden_record(&UndoRecord::Visibility {
            table: None,
            visible_name: "t".into(),
            hidden_name: hidden.clone(),
            is_visible: false,
        })
        .unwrap();
        db.rename_table("other", &hidden).unwrap();
        assert_eq!(db.open_sheet(&hidden).unwrap(), other);
        assert!(db.sheet_name(old).is_err());
    }

    #[test]
    fn expired_deleted_columns_cannot_resurrect_old_column_ids() {
        let mut db =
            database("CREATE TABLE t(value TEXT,other TEXT); INSERT INTO t VALUES('old','new');");
        let sheet = db.open_sheet("t").unwrap();
        let old = db.column_id(sheet, "value").unwrap();
        let other = db.column_id(sheet, "other").unwrap();
        let hidden = db.delete_column("t", "value").unwrap();
        db.discard_hidden_record(&UndoRecord::Visibility {
            table: Some("t".into()),
            visible_name: "value".into(),
            hidden_name: hidden.clone(),
            is_visible: false,
        })
        .unwrap();
        db.rename_column("t", "other", &hidden).unwrap();
        assert_eq!(db.column_id(sheet, &hidden).unwrap(), other);
        assert!(db.column_name(sheet, old).is_err());
    }

    #[test]
    fn snapshot_storage_uses_native_rowids_when_available_and_enforces_private_keys_otherwise() {
        let mut db=database("CREATE TABLE t(value TEXT); INSERT INTO t VALUES('first'); CREATE VIEW v AS SELECT value FROM t; CREATE TABLE shadows(rowid TEXT,_rowid_ TEXT,oid TEXT); INSERT INTO shadows VALUES('a','b','c');");
        let view = db.open_sheet("v").unwrap();
        let name = db.sheet_name(view).unwrap();
        db.execute_batch(&format!("INSERT INTO \"{name}\" VALUES('second')"))
            .unwrap();
        assert_eq!(db.fetch_page(view, 10, 0, "").unwrap().row_ids.len(), 2);
        let copy = db.open_sheet("shadows").unwrap();
        let name = db.sheet_name(copy).unwrap();
        assert!(db
            .execute_batch(&format!(
                "INSERT INTO \"{name}\"(rowid,_rowid_,oid) VALUES('x','y','z')"
            ))
            .is_err());
        assert!(db
            .execute_batch(&format!("UPDATE \"{name}\" SET __dataman_identity=0"))
            .is_err());
        assert_eq!(db.fetch_page(copy, 10, 0, "").unwrap().row_ids.len(), 1);
    }
}
