use std::path::{Path, PathBuf};
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
        id: i32,
        before: DataItem,
        after: DataItem,
        is_after: bool,
    },
    BulkCellEdit {
        table: String,
        column: String,
        cells: Vec<(i32, DataItem, DataItem)>,
        is_after: bool,
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
    ViewChange,
}

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
}

impl Database {
    pub fn new(connection: Connection) -> AppResult<Self> {
        Self::purge_internal_artifacts_from(&connection)?;
        create_table_of_tables(&connection)?;
        if let Err(err) = regexping::custom_functions::add_custom_functions(&connection) {
            log::info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            Ok(Database { connection })
        }
    }
    pub(crate) fn backup_db<P: AsRef<Path>>(&self, dst: P) -> AppResult<()> {
        let mut stmt = self.connection.prepare("PRAGMA page_count")?;
        let page_count: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(i32::MAX);

        let dst_path = dst.as_ref();
        let mut dst = Connection::open(dst_path)?;
        let backup = backup::Backup::new(&self.connection, &mut dst)?;
        backup
            .run_to_completion(page_count, time::Duration::from_millis(250), None)
            .map_err(AppError::from)?;
        drop(backup);
        drop(dst);
        Self::purge_internal_artifacts(dst_path)
    }

    pub fn get(
        &self,
        limit: u32,
        offset: u32,
        table_name: &str,
        ordering: &str,
    ) -> AppResult<DataTable> {
        let headers = self.get_headers(table_name)?;
        let columns = headers
            .iter()
            .map(|h| quote_identifier(h))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT {columns} FROM {} {} LIMIT {} OFFSET {};",
            quote_identifier(table_name),
            ordering,
            limit,
            offset
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
        self.connection
            .query_row(
                &format!(r#"SELECT COUNT(*) FROM "{table_name}";"#),
                [],
                |row| row.get(0),
            )
            .ok()
    }
    pub fn get_cell(&self, table_name: &str, id: i32, header: &str) -> AppResult<String> {
        Ok(self.get_cell_item(table_name, id, header)?.to_string())
    }

    pub(crate) fn get_cell_item(
        &self,
        table_name: &str,
        id: i32,
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
            UndoRecord::BulkCellEdit {
                table,
                column,
                cells,
                is_after,
            } => {
                for (id, before, after) in cells {
                    self.update_cell_value(
                        table,
                        column,
                        *id,
                        if *is_after { before } else { after },
                    )?;
                }
                *is_after = !*is_after;
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
            UndoRecord::ViewChange => {}
        }
        Ok(())
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
        id: i32,
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
        Ok(())
    }

    pub fn execute_batch(&self, sql: &str) -> AppResult<()> {
        let query = &format!(
            r#"BEGIN TRANSACTION;
				{}
			COMMIT;"#,
            sql
        );
        if cfg!(debug_assertions) {
            log::info!("{query}");
        }

        match self.connection.execute_batch(query) {
            Ok(_) => Ok(()),
            Err(err) => {
                self.execute("ROLLBACK;", [])?;
                log::info!("Error executing batch query: {}", err);
                Err(AppError::Sqlite(err))
            }
        }
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
        let query = format!(
            "SELECT {} , {} FROM {}",
            quote_identifier("id"),
            quote_identifier(old_column_name),
            quote_identifier(table_name)
        );
        let mut binding = self.prepare(&query)?;
        let mut rows = binding.query([])?;
        let create_column_query = format!(
            "ALTER TABLE {} ADD COLUMN {} TEXT;",
            quote_identifier(table_name),
            quote_identifier(new_column_name)
        );
        let mut transaction = String::new();
        transaction.push_str(create_column_query.as_ref());
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value = match DataItem::from(row.get_ref(1)?) {
                DataItem::Null => None,
                value => Some(value.to_string()),
            };
            let derived_value = value
                .and_then(|value| fun(value))
                .map(|value| format!("'{}'", value.replace('\'', "''")))
                .unwrap_or_else(|| "NULL".to_owned());
            let update_query = format!(
                "UPDATE {} SET {} = {} WHERE {} = {};",
                quote_identifier(table_name),
                quote_identifier(new_column_name),
                derived_value,
                quote_identifier("id"),
                id
            );
            transaction.push_str(&update_query);
        }
        self.execute_batch(&transaction)?;
        Ok(())
    }

    pub fn get_table_names(&self) -> AppResult<Vec<String>> {
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
        let query = format!(
            "CREATE TABLE {} AS SELECT ROW_NUMBER() OVER () AS id, {} AS value, COUNT(*) AS count FROM {} GROUP BY {} ORDER BY count DESC;",
            quote_identifier(&result), quote_identifier(column), quote_identifier(table), quote_identifier(column)
        );
        self.execute_batch(&query)?;
        Ok(result)
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
        let header_names = self.get_headers(&table_name)?;
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
            regexping::regex_filter_query(header, pattern, &old_table_name, &new_table_name)?;

        let res = self.execute(&query, []);
        res?;
        Ok(new_table_name)
    }

    // go to first match
    pub(crate) fn exact_search(
        &self,
        table_name: &str,
        ordering: &str,
        current_row: u32,
        search_header: &str,
        pattern: &str,
    ) -> AppResult<u32> {
        let query = sql_queries::build::exact_search_query(
            ordering,
            search_header,
            current_row,
            &table_name,
        );

        let row_number: u32 = self
            .connection
            .query_row(&query, [pattern], |row| row.get(0))?;
        Ok(row_number - 1)
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
        log::error!("{} {} {}", table_name, transform_header, new_header_name);
        let queries = regexping::regex_with_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            transformation,
            &table_name,
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
            &table_name,
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
    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.

    pub(crate) fn sql_query(&self, query: &str) -> AppResult<()> {
        self.execute_batch(query)
    }

    pub(crate) fn get_table_name(file: PathBuf) -> Option<String> {
        file.file_stem().map(|el| el.to_string_lossy().into_owned())
    }

    pub fn get_headers(&self, table_name: &str) -> AppResult<Vec<String>> {
        let query = format!(r#"PRAGMA table_info("{}")"#, table_name);
        let mut stmt = self.connection.prepare(&query)?;
        let column_names: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .map(|result| result.expect("Failed to retrieve column name"))
            .filter(|name: &String| !name.starts_with(INTERNAL_PREFIX))
            .collect();
        Ok(column_names)
    }
    pub fn count_headers(&self, table_name: &str) -> AppResult<u16> {
        Ok(self.get_headers(&table_name)?.len() as u16)
    }

    pub fn update_cell(
        &self,
        table_name: &str,
        header: &str,
        id: i32,
        content: &str,
    ) -> AppResult<()> {
        if header == "id" {
            return Err(AppError::from("The id column is read-only"));
        }
        let update_query = format!(r#"UPDATE "{table_name}" SET '{header}' = ? WHERE id = ?;"#);
        self.execute(&update_query, params![content, id])?;
        Ok(())
    }

    pub(crate) fn text_to_int(&self, table_name: &str, column: &str) -> AppResult<()> {
        let queries = sql_queries::build::text_to_int(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn int_to_text(&self, table_name: &str, column: &str) -> AppResult<()> {
        let queries = sql_queries::build::int_to_text_query(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_column(&self, table_name: &str, column: &str) -> AppResult<String> {
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
        let queries = sql_queries::build::rename_column_query(&table_name, &column, new_column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_table(&self, table_name: &str) -> AppResult<String> {
        let hidden = self.internal_table_name()?;
        self.rename_table(table_name, &hidden)?;
        log::info!("Deleted table {table_name}");
        Ok(hidden)
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
        populate_table_of_tables(&self.connection)?;
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

        let summary = database.summary_table("sample", "value").unwrap();
        let row = database.get(1, 0, &summary, "").unwrap().1.remove(0);
        assert_eq!(row.get(1), Some(&DataItem::Integer(4)));
        assert_eq!(row.get(2), Some(&DataItem::Integer(1)));
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
        assert!(database.get_cell("data", i32::MAX, "firstname").is_err());
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
            .derive_column("t", "count", "derived", |value| Some(value))
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
