use crate::controller;
use crate::controller::command::{Command, PreviousCommand, QueuedCommand};
use crate::controller::input::StateMachine;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::process::id;
use std::{slice, time};

use crossterm::ExecutableCommand;
use ratatui::widgets::TableState;
use regex::Regex;
use rusqlite::types::ValueRef;
use rusqlite::{backup, params, Connection, Error, Statement};

use crate::app_error_other;
use crate::error::{AppError, AppResult};
use crate::model::converter::insert_csv_data_database;
use crate::model::datarow::DataItem;
use crate::tui::TUI;

use super::datarow::DataTable;
use super::db_slice::DatabaseSlice;
use super::metadata::{create_table_of_tables, populate_table_of_tables};
use super::regexping;
use super::{converter, sql_queries};

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
    pub(crate) header_idx: u16,
    pub(crate) order_column: Option<String>,
    pub(crate) is_asc_order: bool,
    pub(crate) current_table_idx: u16,
    pub(crate) slice: DatabaseSlice,
    pub(crate) input: String,
    pub(crate) character_index: usize,
    pub(crate) last_command: PreviousCommand,
    pub(crate) queued_command: Option<QueuedCommand>,
    pub(crate) input_mode_state_machine: StateMachine,
    // regex_map: HashMap<String, Regex>,
}

impl Database {
    pub fn new(connection: Connection) -> AppResult<Self> {
        let query = r#"SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;"#
            .to_string();
        let rowid: u16 = connection.query_row(&query, [], |row| row.get(0))?;

        let mut table_state = TableState::new();
        table_state.select(Some(0));
        let slice = DatabaseSlice::new(vec![], vec![], table_state, 0, 0);
        create_table_of_tables(&connection)?;
        if let Err(err) = regexping::custom_functions::add_custom_functions(&connection) {
            log::info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            Ok(Database {
                connection,
                header_idx: 0,
                order_column: Some("id".to_string()),
                is_asc_order: true,
                current_table_idx: rowid,
                slice,
                input: String::new(),
                character_index: 0,
                last_command: PreviousCommand::new(Command::None, None),
                queued_command: None,
                input_mode_state_machine: StateMachine::new(),
            })
        }
    }
    pub(crate) fn backup_db<P: AsRef<Path>>(&self, dst: P) -> AppResult<()> {
        let mut stmt = self.connection.prepare("PRAGMA page_count")?;
        let page_count: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(i32::MAX);

        let mut dst = Connection::open(dst)?;
        let backup = backup::Backup::new(&self.connection, &mut dst)?;
        backup
            .run_to_completion(page_count, time::Duration::from_millis(250), None)
            .map_err(AppError::from)
    }
    pub fn get_current_header(&self) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
        self.get_headers(&table_name)?
            .get(self.header_idx as usize)
            .cloned()
            .ok_or(app_error_other!("Could not get header"))
    }

    fn get_ordering(&self) -> String {
        let ordering = if self.is_asc_order { "ASC" } else { "DESC" };
        match &self.order_column {
            Some(order_column) => format!(r#" ORDER BY "{}" {} "#, order_column, ordering),
            None => "".to_string(),
        }
    }
    pub fn get(&mut self, limit: u32, offset: u32, table_name: String) -> AppResult<DataTable> {
        if self.slice.is_unchanged() {
            return Ok((self.slice.headers.clone(), self.slice.data_rows.clone()));
        }
        let query = format!(
            r#"SELECT * FROM "{}" {} LIMIT {} OFFSET {};"#,
            table_name,
            self.get_ordering(),
            limit,
            offset
        );

        let (headers, data_rows) = {
            let mut data_rows = vec![];
            let mut stmt = self.prepare(&query).unwrap();
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
        self.slice.data_rows.clone_from(&data_rows);
        self.slice.headers.clone_from(&headers);
        self.slice.is_unchanged = true;
        Ok((headers, data_rows))
    }
    pub(crate) fn count_rows(&self) -> Option<u32> {
        let table_name = self.get_current_table_name().ok()?;
        self.connection
            .query_row(
                &format!(r#"SELECT COUNT(*) FROM "{table_name}";"#),
                [],
                |row| row.get(0),
            )
            .ok()
    }
    pub fn get_cell(&self, id: i32, header: &str) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
        let query = format!(r#"SELECT "{}" FROM "{}" WHERE id = ?;"#, header, table_name);
        let mut stmt = self.prepare(&query)?;
        log::info!("id: {}", id);
        let mut rows = stmt.query(params![id])?;
        let row = rows.next()?.unwrap();
        let cell = row.get(0)?;

        Ok(cell)
    }
    fn prepare(&self, sql: &str) -> rusqlite::Result<Statement> {
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
        old_column_name: &str,
        new_column_name: &str,
        fun: F,
    ) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        let table_name = self.get_current_table_name()?;
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let query = format!(r#"SELECT "id", "{old_column_name}" FROM "{table_name}""#);
        let mut binding = self.prepare(&query)?;
        let mut rows = binding.query([])?;
        let create_column_query =
            format!(r#"ALTER TABLE "{table_name}" ADD COLUMN '{new_column_name}' TEXT;"#);
        dbg!(&create_column_query);
        // return Ok(());
        let mut transaction = String::new();
        transaction.push_str(create_column_query.as_ref());
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value).unwrap_or("NULL".to_string()).replace('\'', "''");
            let update_query = format!(
                r#"UPDATE "{table_name}" SET "{new_column_name}" = '{derived_value}' WHERE id = '{id}';"#,
            );
            transaction.push_str(&update_query);
        }
        self.execute_batch(&transaction)?;
        Ok(())
    }

    pub(crate) fn get_current_id(&self) -> AppResult<i32> {
        let i = self.slice.table_state.selected().unwrap_or(0);
        let query = format!(
            r#"SELECT rowid FROM "{}" LIMIT 1 OFFSET {};"#,
            self.get_current_table_name()?,
            i
        );
        let id: i32 = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(id)
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        // sort by current header
        let header = self.get_current_header()?;
        self.slice.table_state.select(Some(0));
        if self.order_column == Some(header.clone())
        //|| (self.order_column.is_none() && (header == "id"))
        {
            self.is_asc_order = !self.is_asc_order;
        } else {
            self.is_asc_order = true;
            self.order_column = Some(header);
        }

        Ok(())
    }
    pub fn get_table_names(&self) -> AppResult<Vec<String>> {
        let query = r#"SELECT name FROM sqlite_master WHERE type='table' ORDER BY rowid;"#;
        let mut stmt = self.prepare(query)?;
        let mut rows = stmt.query([])?;
        let mut table_names = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            table_names.push(name);
        }
        Ok(table_names)
    }
    pub fn get_current_table_name(&self) -> AppResult<String> {
        let query = format!(
            r#"SELECT name FROM sqlite_master WHERE type='table' AND rowid='{}';"#,
            self.current_table_idx
        );
        let table_name = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(table_name)
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

    fn find_unused_header_name(&self, header_name: &str) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
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
    pub fn regex_filter(&mut self, header: &str, pattern: &str) -> AppResult<()> {
        // create new table with filter applied using create table as sqlite statement.
        let old_table_name = self.get_current_table_name()?;
        let temp_table_name = format!("{}_filt", old_table_name);
        let new_table_name = self.find_unused_table_name(&temp_table_name)?;
        let (query, new_table_name) =
            regexping::regex_filter_query(header, pattern, &old_table_name, &new_table_name)?;

        let res = self.execute(&query, []);
        res?;
        self.select_table(&new_table_name)?;
        Ok(())
    }

    // go to first match
    pub(crate) fn exact_search(&mut self, search_header: &str, pattern: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let current_row =
            self.slice.row_offset + self.slice.table_state.selected().unwrap_or(0) as u32;
        let query = sql_queries::build::exact_search_query(
            &self.get_ordering(),
            search_header,
            current_row,
            &table_name,
        );

        let row_number: u32 = self
            .connection
            .query_row(&query, [pattern], |row| row.get(0))?;
        let row_number = row_number - 1;
        let height = TUI::get_table_height()?;
        let row_idx = row_number % height;
        let row_offset = row_number - row_idx;

        self.slice.update(row_idx, row_offset);
        Ok(())
    }

    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.
    pub(crate) fn regex_capture_group_transform(
        &self,
        pattern: &str,
        header: &str,
        transformation: &str,
    ) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(&transform_header)?;
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
        pattern: &str,
        header: &str,
    ) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let transform_header = format!("{}_tform", header);
        let new_header_name = self.find_unused_header_name(&transform_header)?;
        let queries = regexping::regex_no_capture_group_transform_query(
            header,
            &new_header_name,
            pattern,
            &table_name,
        )?;
        self.execute_batch(&queries)?;
        Ok(())
    }

    pub(crate) fn copy(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let header = self.get_current_header()?;
        let transform_header = format!("{}_copy", header);
        let new_header_name = self.find_unused_header_name(&transform_header)?;
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
            .collect();
        Ok(column_names)
    }
    pub fn count_headers(&self) -> AppResult<u16> {
        let table_name = self.get_current_table_name()?;
        Ok(self.get_headers(&table_name)?.len() as u16)
    }

    pub(crate) fn move_cursor(
        &mut self,
        direction: controller::direction::Direction,
    ) -> AppResult<()> {
        match direction {
            controller::direction::Direction::Left => self.previous_header()?,
            controller::direction::Direction::Right => self.next_header()?,
            controller::direction::Direction::Up => self.previous_row()?,
            controller::direction::Direction::Down => self.next_row()?,
        }
        Ok(())
    }
    fn next_header(&mut self) -> AppResult<()> {
        self.header_idx = (self.header_idx + 1) % self.count_headers()?;
        Ok(())
    }
    fn previous_header(&mut self) -> AppResult<()> {
        if self.header_idx == 0 {
            self.header_idx = self.count_headers()?;
        };
        self.header_idx -= 1;
        Ok(())
    }

    fn next_row(&mut self) -> AppResult<()> {
        let slice = &mut self.slice;
        let row_idx = slice.table_state.selected().unwrap_or(0) as u32;
        let row_offset = slice.row_offset;
        let height = TUI::get_table_height()?;
        let l = self.slice.data_rows.len();
        let val = row_idx + row_offset;
        let is_last_page = height > l as u32;

        if is_last_page && row_idx + 2 > l as u32 {
            return Ok(());
        }
        let i = match self.slice.table_state.selected() {
            Some(i) if i < (height - 1) as usize => i + 1,
            Some(i) if i >= (height - 1) as usize => {
                let max = self.count_rows().unwrap_or(u32::MAX);
                if (self.slice.row_offset + i as u32) < max {
                    self.slice.row_offset = self.slice.row_offset.saturating_add(height);
                    self.slice.has_changed();
                    0
                } else {
                    i
                }
            }
            _ => 0,
        };

        self.slice.table_state.select(Some(i));
        Ok(())
    }

    fn set_current_row(&mut self, value: usize) {
        self.slice.table_state.select(Some(value));
    }
    fn previous_row(&mut self) -> AppResult<()> {
        let i = match self.slice.table_state.selected() {
            Some(i) if i == 0 && self.slice.row_offset != 0 => {
                let height = TUI::get_table_height()?;
                self.slice.row_offset = self.slice.row_offset.saturating_sub(height);
                self.slice.has_changed();
                height as usize - 1
            }

            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.slice.table_state.select(Some(i));
        Ok(())
    }
    pub fn update_cell(&self, header: &str, id: i32, content: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let update_query = format!(r#"UPDATE "{table_name}" SET '{header}' = ? WHERE id = ?;"#);
        self.execute(&update_query, params![content, id])?;
        Ok(())
    }

    pub fn next_table(&mut self) -> AppResult<()> {
        let query = format!(
            r#"SELECT rowid FROM sqlite_master WHERE type='table' AND rowid > '{}' AND name != 'table_of_tables' ORDER BY rowid ASC LIMIT 1;"#,
            self.current_table_idx
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        self.slice.row_offset = 0;
        self.slice.table_state.select(Some(0));
        Ok(())
    }
    pub fn select_table(&mut self, table_name: &str) -> AppResult<()> {
        let query = format!(
            r#"SELECT rowid FROM sqlite_master WHERE type='table' AND name = '{table_name}';"#
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        self.slice.row_offset = 0;
        self.slice.table_state.select(Some(0));
        Ok(())
    }

    pub(crate) fn prev_table(&mut self) -> AppResult<()> {
        let query = format!(
            r#"SELECT rowid FROM sqlite_master WHERE type='table' AND rowid < {} AND name != 'table_of_tables' ORDER BY rowid DESC LIMIT 1;"#,
            self.current_table_idx
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        self.slice.row_offset = 0;
        self.slice.table_state.select(Some(0));
        Ok(())
    }

    pub(crate) fn text_to_int(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let column = self.get_current_header()?;
        let queries = sql_queries::build::text_to_int(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn int_to_text(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let column = self.get_current_header()?;
        let queries = sql_queries::build::int_to_text_query(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_column(&mut self) -> AppResult<Option<String>> {
        let table_name = self.get_current_table_name()?;
        let order_column = &self.order_column;
        let column = self.get_current_header()?;
        if Some(&column) == order_column.as_ref() {
            self.order_column = None;
        }
        let queries = sql_queries::build::delete_column_query(&table_name, &column);
        self.execute_batch(&queries)?;
        self.header_idx = self.header_idx.saturating_sub(1);
        Ok(Some(format!("Deleted column {column} from {table_name}")))
    }

    pub(crate) fn rename_column(&mut self, new_column: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let order_column = &self.order_column;
        let column = self.get_current_header()?;
        if Some(&column) == order_column.as_ref() {
            self.order_column = Some(new_column.to_string());
        }
        let queries = sql_queries::build::rename_column_query(&table_name, &column, new_column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_table(&mut self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let query = sql_queries::build::delete_table_query(&table_name);
        self.execute(&query, [])?;
        log::info!("Deleted table {table_name}");
        self.prev_table()?;
        Ok(())
    }

    pub(crate) fn rename_table(&self, new_table_name: &str) -> AppResult<()> {
        let old_table_name = &self.get_current_table_name()?;
        let query = sql_queries::build::rename_table_query(old_table_name, new_table_name);
        self.execute(&query, [])?;
        Ok(())
    }

    // TODO if the column contains a float, 3.0, then ensure that ALL intermediary calculations are done with floats.
    // currently (3/2)*2.0 = 2.0, but it should be 3.0.
    pub(crate) fn math_operation(&self, inputs: Vec<String>) -> AppResult<()> {
        let math_expr = inputs[0].clone();
        let new_math_expr_col = self.find_unused_header_name("math_expr")?;
        let query = sql_queries::build::math_expression_query(
            &new_math_expr_col,
            &self.get_current_table_name()?,
            &math_expr,
        );
        self.execute_batch(&query)
    }

    pub(crate) fn view_metadata_table(&mut self) -> Result<(), AppError> {
        let current_tbl_name = self.get_current_table_name()?;
        if current_tbl_name == "table_of_tables" {
            let prev_tbl_res = self.prev_table();
            let next_tbl_res = self.next_table();
            prev_tbl_res.and(next_tbl_res)
        } else {
            populate_table_of_tables(&self.connection)?;
            self.select_table("table_of_tables")
        }
    }
}

impl TryFrom<Vec<PathBuf>> for Database {
    type Error = AppError;

    fn try_from(paths: Vec<PathBuf>) -> Result<Self, AppError> {
        if paths.is_empty() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No file paths provided",
            )));
        }
        let database_result = if paths.len() == 1 {
            let path = paths
                .first()
                .ok_or(AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "There is no first path",
                )))?
                .clone();
            match path.extension().and_then(|s| s.to_str()) {
                Some("csv") => {
                    let connection = if cfg!(debug_assertions) {
                        log::info!("Debug mode, opening in memory db.");
                        Connection::open_in_memory()?
                    } else {
                        log::info!("Release mode, saving to file 'db.sqlite'.");
                        let _ = std::fs::remove_file("db.sqlite");
                        Connection::open("db.sqlite")?
                    };
                    let database = converter::database_from_csv(path, connection)?;
                    Ok(database)
                }
                Some("sqlite") | Some("sqlite3") => {
                    let connection = Connection::open(path)?;
                    let database = converter::database_from_sqlite(connection)?;
                    Ok(database)
                }
                _ => Err(AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid file extension",
                ))),
            }
        } else if paths
            .iter()
            .map(|p| p.extension())
            .all(|el| el.is_some_and(|el| el == "csv"))
        {
            let _ = std::fs::remove_file("db.sqlite");
            let connection = Connection::open("db.sqlite")?;
            let database = converter::database_from_csv(
                paths
                    .first()
                    .ok_or(app_error_other!("There is no path"))?
                    .clone(),
                connection,
            )?;
            for path in paths.iter().skip(1) {
                insert_csv_data_database(path.clone(), &database.connection)?;
            }
            Ok(database)
        } else {
            Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid file extension. One or several csv files or a single sqlite3 database can be provided.")))
        };
        let database = database_result?;
        // populate_table_of_tables(&database.connection)?;
        Ok(database)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use controller::direction::Direction;

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
        let number_of_headers = database.count_headers().unwrap();
        assert_eq!(number_of_headers, 4)
    }

    #[test]
    fn inc_header() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database.next_header().unwrap();
        assert_eq!(1, database.header_idx);
        database.next_header().unwrap();
        assert_eq!(2, database.header_idx);
        database.next_header().unwrap();
        assert_eq!(3, database.header_idx);
        database.next_header().unwrap();
        assert_eq!(0, database.header_idx);
    }

    #[test]
    fn dec_header() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database.previous_header().unwrap();
        assert_eq!(3, database.header_idx);
        database.previous_header().unwrap();
        assert_eq!(2, database.header_idx);
        database.previous_header().unwrap();
        assert_eq!(1, database.header_idx);
        database.previous_header().unwrap();
        assert_eq!(0, database.header_idx);
    }

    #[test]
    fn derive_column_test() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let col = "firstname";
        let col_new = "firstname_derived";
        let fun = |s| Some(format!("{}-changed", s));
        database.derive_column(col, col_new, fun).unwrap();
        let first: String = database.get(1, 0, "data".to_string()).unwrap().1[0]
            .get(4)
            .unwrap()
            .to_string();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers().unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn update_cell() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        database.update_cell("firstname", 1, "hank").unwrap();
        let first: String = database.get(1, 0, "data".to_string()).unwrap().1[0]
            .get(1)
            .unwrap()
            .to_string();

        let (_, rows) = database.get(1, 0, "data".to_string()).unwrap();

        // assert_eq!(first, "hank");
    }

    #[test]
    fn test_offset() {
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();

        // we use limit 2 so we can check first and second item
        let (headers, data_rows) = database.get(2, 0, "data".to_string()).unwrap();
        let first: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("henrik", first);

        // we can look into the old data rows to see the next item:
        let second: String = data_rows[1].get(1).unwrap().to_string();
        assert_eq!("john", second);

        // or we can update the 'cache'
        // this makes us get a new view into the database
        // if we don't use this we would need to index into [1] instead
        database.slice.has_changed();
        let (headers, data_rows) = database.get(1, 1, "data".to_string()).unwrap();
        let second: String = data_rows[0].get(1).unwrap().to_string();
        assert_eq!("john", second);
    }

    #[test]
    fn get_table_names_test() {
        let database = setup_database();
        let table_names = database.get_table_names().unwrap();
    }

    #[test]
    fn get_current_table_name_test() {
        let database = setup_database();
        let table_name = database.get_current_table_name().unwrap();
        assert_eq!(table_name, "data".to_string());
    }

    #[test]
    fn get_headers_test() {
        let database = setup_database();
        let table_name = database.get_current_table_name().unwrap();
        let headers = database.get_headers(&table_name).unwrap();
        assert_eq!(headers, vec!["id", "firstname", "lastname", "age"]);
    }

    #[test]
    fn count_rows_test() {
        let database = setup_database();
        let rows_len = database.count_rows().unwrap();
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
    fn select_table_test() {
        let mut database = setup_three_table_db();
        let table_name = database.get_current_table_name().unwrap();
        assert!(table_name == "t1");
        database.select_table("t3").unwrap();
        let table_name = database.get_current_table_name().unwrap();
        assert!(table_name == "t3");
    }

    #[test]
    fn table_of_tables_test() {
        let database = setup_three_table_db();
    }

    #[test]
    fn copy_column_long_test() {
        let p = vec![PathBuf::from("assets/data-long.csv")];
        let mut database = Database::try_from(p).unwrap();

        let copy_fun = |s: String| Some(s.to_string());
        database.move_cursor(Direction::Right).unwrap();

        let column_name = database.get_current_header().unwrap();
        let new_column_name = database.find_unused_header_name(&column_name).unwrap();
        database
            .derive_column(&column_name, &new_column_name, copy_fun)
            .unwrap();
        let headers = database
            .get_headers(&database.get_current_table_name().unwrap())
            .unwrap();
        let table_name = database.get_current_table_name().unwrap();
        let (_, res) = database.get(20, 0, table_name).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }

    #[test]
    fn copy_column_test() {
        let p = vec![PathBuf::from("assets/data.csv")];
        let mut database = Database::try_from(p).unwrap();
        let copy_fun = |s: String| Some(s.to_string());

        database.move_cursor(Direction::Right).unwrap();
        let column_name = database.get_current_header().unwrap();
        let new_column_name = database.find_unused_header_name(&column_name).unwrap();
        database
            .derive_column(&column_name, &new_column_name, copy_fun)
            .unwrap();
        let (_, res) = database.get(20, 100, "data".to_string()).unwrap();
        for row in res.iter() {
            let original = row.get(1);
            let copy = row.get(4);
            assert_eq!(original, copy);
        }
    }
}
