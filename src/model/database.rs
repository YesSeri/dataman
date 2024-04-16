use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time;

use crossterm::ExecutableCommand;
use log::{error, info};
use ratatui::widgets::TableState;
use rusqlite::types::ValueRef;
use rusqlite::{backup, params, Connection, Statement};

use crate::error::{AppError, AppResult};
use crate::model::converter::insert_csv_data_database;
use crate::model::datarow::DataItem;
use crate::tui::TUI;

use super::datarow::DataTable;
use super::db_slice::DatabaseSlice;
use super::regexping;
use super::{converter, query_builder};

#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
    pub(crate) header_idx: u16,
    pub(crate) order_column: Option<String>,
    pub(crate) is_asc_order: bool,
    pub(crate) current_table_idx: u16,
    pub(crate) slices: Vec<DatabaseSlice>,
}

impl Database {
    pub fn new(connection: Connection) -> AppResult<Self> {
        let query = "SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;"
            .to_string();
        let rowid: u16 = connection.query_row(&query, [], |row| row.get(0))?;

        let mut table_state = TableState::new();
        table_state.select(Some(0));
        let slices = vec![DatabaseSlice::new(vec![], vec![], table_state, 0, 0)];

        let database = Database {
            connection,
            header_idx: 0,
            order_column: Some("id".to_string()),
            is_asc_order: true,
            current_table_idx: rowid,
            slices,
        };
        if let Err(err) = regexping::custom_functions::add_custom_functions(&database) {
            info!("Error adding custom functions, e.g. REGEXP: {}", err);
            Err(AppError::Sqlite(err))
        } else {
            Ok(database)
        }
    }
    pub(crate) fn backup_db<P: AsRef<Path>>(&self, dst: P) -> rusqlite::Result<()> {
        let mut stmt = self.connection.prepare("PRAGMA page_count")?;
        let page_count: i32 = stmt.query_row([], |row| row.get(0)).unwrap_or(i32::MAX);

        let mut dst = Connection::open(dst)?;
        let backup = backup::Backup::new(&self.connection, &mut dst)?;
        backup.run_to_completion(page_count, time::Duration::from_millis(250), None)
    }
    pub fn get_current_header(&self) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
        self.get_headers(&table_name)?
            .get(self.header_idx as usize)
            .cloned()
            .ok_or(AppError::Other)
    }

    fn get_ordering(&self) -> String {
        let ordering = if self.is_asc_order { "ASC" } else { "DESC" };
        match &self.order_column {
            Some(order_column) => format!(" ORDER BY `{}` {} ", order_column, ordering),
            None => "".to_string(),
        }
    }
    pub fn get(&mut self, limit: u32, offset: u32, table_name: String) -> AppResult<DataTable> {
        if self.slices[0].is_unchanged() {
            Ok((
                self.slices[0].headers.clone(),
                self.slices[0].data_rows.clone(),
            ))
        } else {
            let query = format!(
                "SELECT * FROM `{}` {} LIMIT {} OFFSET {};",
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
            self.slices[0].data_rows = data_rows.clone();
            self.slices[0].headers = headers.clone();
            self.slices[0].is_unchanged = true;
            Ok((headers, data_rows))
        }
    }
    pub(crate) fn count_rows(&self) -> Option<u32> {
        let table_name = self.get_current_table_name().ok()?;
        self.connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {};", table_name),
                [],
                |row| row.get(0),
            )
            .ok()
    }
    pub fn get_cell(&self, id: i32, header: &str) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
        let query = format!("SELECT `{}` FROM `{}` WHERE id = ?;", header, table_name);
        let mut stmt = self.prepare(&query)?;
        info!("id: {}", id);
        let mut rows = stmt.query(params![id])?;
        let row = rows.next()?.unwrap();
        let cell = row.get(0)?;

        Ok(cell)
    }
    fn prepare(&self, sql: &str) -> rusqlite::Result<Statement> {
        info!("{sql}");
        self.connection.prepare(sql)
    }
    fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> AppResult<()> {
        if cfg!(debug_assertions) {
            info!("{sql}");
        }
        self.connection.execute(sql, params)?;
        Ok(())
    }

    pub(super) fn execute_batch(&self, sql: &str) -> AppResult<()> {
        let query = &format!("BEGIN TRANSACTION;\n{}\nCOMMIT;", sql);
        if cfg!(debug_assertions) {
            info!("{query}");
        }

        match self.connection.execute_batch(query) {
            Ok(_) => Ok(()),
            Err(err) => {
                self.execute("ROLLBACK;", [])?;
                error!("Error executing batch query: {}", err);
                Err(AppError::Sqlite(err))
            }
        }
    }
    pub fn derive_column<F>(&self, column_name: String, fun: F) -> AppResult<()>
    where
        F: Fn(String) -> Option<String>,
    {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        let table_name = self.get_current_table_name()?;
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let query = format!("SELECT `id`, `{column_name}` FROM `{table_name}`");
        let mut binding = self.prepare(&query)?;
        let mut rows = binding.query([])?;
        let derived_column_name = format!("derived{}", column_name);
        let create_column_query =
            format!("ALTER TABLE `{table_name}` ADD COLUMN `{derived_column_name}` TEXT;\n");
        let mut transaction = String::new();
        transaction.push_str(create_column_query.as_ref());
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value).unwrap_or("NULL".to_string());
            let update_query = format!(
                "UPDATE `{table_name}` SET '{derived_column_name}' = '{derived_value}' WHERE id = '{id}';\n",
            );
            transaction.push_str(&update_query);
        }
        self.execute_batch(&transaction)?;
        Ok(())
    }

    pub(crate) fn get_current_id(&self) -> AppResult<i32> {
        let i = self.slices[0].table_state.selected().unwrap_or(0);
        let query = format!(
            "SELECT rowid FROM `{}` LIMIT 1 OFFSET {};",
            self.get_current_table_name()?,
            i
        );
        let id: i32 = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(id)
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        // sort by current header
        let header = self.get_current_header()?;
        self.slices[0].table_state.select(Some(0));
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
        let query = "SELECT name FROM sqlite_master WHERE type='table' ORDER BY rowid;";
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
            "SELECT name FROM sqlite_master WHERE type='table' AND rowid='{}';",
            self.current_table_idx
        );
        let table_name = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(table_name)
    }
    pub fn regex_filter(&mut self, header: &str, pattern: &str) -> AppResult<()> {
        // create new table with filter applied using create table as sqlite statement.
        let table_name = self.get_current_table_name()?;
        let query = regexping::build_regex_filter_query(header, pattern, &table_name)?;

        self.execute(&query, [])?;
        self.next_table()?;
        Ok(())
    }

    // go to first match
    pub(crate) fn exact_search(&mut self, search_header: &str, pattern: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let current_row =
            self.slices[0].row_offset + self.slices[0].table_state.selected().unwrap_or(0) as u32;
        let query = query_builder::build_exact_search_query(
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
        let row_idx = row_number % height as u32;
        let row_offset = row_number - row_idx;

        self.slices[0].update(row_idx, row_offset);
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
        let queries = regexping::build_regex_with_capture_group_transform_query(
            header,
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
        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let queries =
            regexping::build_regex_no_capture_group_transform_query(header, pattern, &table_name)?;
        self.execute_batch(&queries)?;
        Ok(())
    }

    pub(crate) fn copy(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let header = self.get_current_header()?;
        let derived_header_name = format!("derived{}", header);
        let create_header_query =
            format!("ALTER TABLE `{table_name}` ADD COLUMN `{derived_header_name}` TEXT;\n");

        let mut queries = String::new();
        queries.push_str(&create_header_query);
        let update_query =
            format!("UPDATE `{table_name}` SET `{derived_header_name}` = `{header}`;");
        queries.push_str(&update_query);
        self.execute_batch(&queries)
    }
    /// This is a regex capture without capture groups e.g. [g-k].*n.
    /// Get the first capture that matches the pattern, a letter between g and k, followed by any number of characters, followed by n.

    pub(crate) fn sql_query(&self, query: String) -> AppResult<()> {
        self.execute_batch(&query)
    }

    pub(crate) fn get_table_name(file: PathBuf) -> String {
        file.file_stem().unwrap().to_str().unwrap().to_string()
    }

    pub fn get_headers(&self, table_name: &str) -> AppResult<Vec<String>> {
        let query = format!("PRAGMA table_info(`{}`)", table_name);
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

    pub(crate) fn move_cursor(&mut self, direction: crate::controller::Direction) -> AppResult<()> {
        match direction {
            crate::controller::Direction::Left => self.previous_header()?,
            crate::controller::Direction::Right => self.next_header()?,
            crate::controller::Direction::Up => self.previous_row()?,
            crate::controller::Direction::Down => self.next_row()?,
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
        let height = TUI::get_table_height()?;
        let i = match self.slices[0].table_state.selected() {
            Some(i) if i < (height - 1) as usize => i + 1,
            Some(i) if i >= (height - 1) as usize => {
                let max = self.count_rows().unwrap_or(u32::MAX);
                if (self.slices[0].row_offset + i as u32) < max {
                    self.slices[0].row_offset =
                        self.slices[0].row_offset.saturating_add(height as u32);
                    self.slices[0].has_changed();
                    0
                } else {
                    i
                }
            }
            _ => 0,
        };

        self.slices[0].table_state.select(Some(i));
        Ok(())
    }

    fn set_current_row(&mut self, value: usize) {
        self.slices[0].table_state.select(Some(value));
    }
    fn previous_row(&mut self) -> AppResult<()> {
        let i = match self.slices[0].table_state.selected() {
            Some(i) if i == 0 && self.slices[0].row_offset != 0 => {
                let height = TUI::get_table_height()?;
                self.slices[0].row_offset = self.slices[0].row_offset.saturating_sub(height as u32);
                self.slices[0].has_changed();
                height as usize - 1
            }

            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.slices[0].table_state.select(Some(i));
        Ok(())
    }
    pub fn update_cell(&self, header: &str, id: i32, content: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let update_query = format!("UPDATE `{}` SET '{}' = ? WHERE id = ?;", table_name, header);
        self.execute(&update_query, params![content, id])?;
        Ok(())
    }

    pub fn next_table(&mut self) -> AppResult<()> {
        let query = format!(
            "SELECT rowid FROM sqlite_master WHERE type='table' AND rowid > {} ORDER BY rowid ASC LIMIT 1;",
            self.current_table_idx
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(())
    }

    pub(crate) fn prev_table(&mut self) -> AppResult<()> {
        let query = format!(
            "SELECT rowid FROM sqlite_master WHERE type='table' AND rowid < {} ORDER BY rowid DESC LIMIT 1;",
            self.current_table_idx
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(())
    }

    pub(crate) fn text_to_int(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let column = self.get_current_header()?;
        let queries = query_builder::build_text_to_int(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn int_to_text(&self) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let column = self.get_current_header()?;
        let queries = query_builder::build_int_to_text_query(&table_name, &column);
        self.execute_batch(&queries)
    }

    pub(crate) fn delete_column(&mut self) -> AppResult<Option<String>> {
        let table_name = self.get_current_table_name()?;
        let order_column = &self.order_column;
        let column = self.get_current_header()?;
        if Some(&column) == order_column.as_ref() {
            self.order_column = None;
        }
        let queries = query_builder::build_delete_column_query(&table_name, &column);
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
        let queries = query_builder::build_rename_column_query(&table_name, &column, new_column);
        self.execute_batch(&queries)
    }
    pub(crate) fn open_table_of_tables(&self) -> AppResult<String> {
        let query = "SELECT name FROM sqlite_master WHERE type='table';".to_string();
        let mut stmt = self.connection.prepare(&query)?;
        let mut rows = stmt.query([])?;

        let mut names: Vec<String> = Vec::new();
        while let Some(row) = rows.next()? {
            names.push(row.get(0)?);
        }
        Ok("aaa".to_string())
    }
}

impl TryFrom<Vec<PathBuf>> for Database {
    type Error = AppError;

    fn try_from(paths: Vec<PathBuf>) -> Result<Self, AppError> {
        if paths.len() == 1 {
            let extension = paths
                .first()
                .unwrap()
                .extension()
                .unwrap()
                .to_str()
                .unwrap();

            let path = paths.first().unwrap().clone();
            match extension {
                "csv" => {
                    let _ = std::fs::remove_file("db.sqlite");
                    let connection = Connection::open("db.sqlite").unwrap();
                    // let connection = Connection::open_in_memory().unwrap();
                    let database = converter::database_from_csv(path, connection)?;
                    Ok(database)
                }
                "sqlite" | "sqlite3" => {
                    let connection = Connection::open(path).unwrap();

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
            let connection = Connection::open("db.sqlite").unwrap();
            let database =
                converter::database_from_csv(paths.first().unwrap().clone(), connection)?;
            for path in paths.iter().skip(1) {
                insert_csv_data_database(path.clone(), &database.connection)?;
            }
            Ok(database)
        } else {
            Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid file extension",
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn get_number_of_headers_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let number_of_headers = database.count_headers();
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
        let mut database = Database::try_from(vec![PathBuf::from("assets/data.csv")]);
        let col = "firstname";
        let fun = |s| Some(format!("{}-changed", s));
        let mut database = database.unwrap();
        database.derive_column(col.to_string(), fun).unwrap();
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
        let first: String = database.get(1, 0, "data".to_string()).unwrap().1[0]
            .get(1)
            .unwrap()
            .to_string();
        assert_eq!("henrik", first);
        let second: String = database.get(1, 2, "data".to_string()).unwrap().1[0]
            .get(1)
            .unwrap()
            .to_string();
        assert_eq!("john", second);
    }

    #[test]
    fn get_table_names_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let table_names = database.get_table_names().unwrap();
    }

    #[test]
    fn get_current_table_name_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let table_name = database.get_current_table_name().unwrap();
        assert_eq!(table_name, "data".to_string());
    }

    #[test]
    fn get_headers_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let table_name = database.get_current_table_name().unwrap();
        let headers = database.get_headers(&table_name).unwrap();
        assert_eq!(headers, vec!["id", "firstname", "lastname", "age"]);
    }

    #[test]
    fn count_rows_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        let rows_len = database.count_rows().unwrap();
        assert_eq!(rows_len, 6)
    }

    #[test]
    fn custom_functions_regexp_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        // let query = "SELECT firstname FROM `data` WHERE firstname REGEXP 'hen'";
        let query = "SELECT firstname FROM `data` WHERE regexp('h.*k', firstname)";
        let result: String = database
            .connection
            .query_row(query, [], |row| row.get(0))
            .unwrap();
        assert_eq!(result, "henrik");
    }

    #[test]
    fn table_of_tables_test() {
        let database = Database::try_from(vec![PathBuf::from("assets/db.sqlite")]).unwrap();
        let s = database.open_table_of_tables().unwrap();
        // assert_eq!(false, true);
    }

    // #[test]
    // fn custom_functions_regexp_transform_no_capture_test() {
    //     let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
    //     let table_name = database.get_current_table_name().unwrap();
    //     let header = database.get_headers(&table_name).unwrap()[1].clone();
    //     let pattern = "n.*";
    //
    //     let query =
    //         regexping::build_regex_no_capture_group_transform_query(&header, pattern, &table_name)
    //             .unwrap();
    //
    //     database.execute_batch(&query).unwrap();
    //
    //     let result: String = database
    //         .connection
    //         .query_row(
    //             "SELECT derivedfirstname FROM `data` WHERE id = 1 ORDER BY rowid ASC",
    //             [],
    //             |row| row.get(0),
    //         )
    //         .unwrap();
    //     assert_eq!(result, "nrik");
    // let query = "SELECT regexp_simple('h.*r', 'henrik')";
    // let mut stmt = database.prepare(query).unwrap();
    // let mut rows = stmt.query([]).unwrap();
    // let row = rows.next().unwrap().unwrap();
    // let result: String = row.get(0).unwrap();
    // assert_eq!(result, "henr");

    // let query = "SELECT regexp_transform('.*ri', 'henrik')";
    // let mut stmt = database.prepare(query).unwrap();
    // let mut rows = stmt.query([]).unwrap();
    // let row = rows.next().unwrap().unwrap();
    // let result: String = row.get(0).unwrap();
    // assert_eq!(result, "heri");

    // let query = "SELECT regexp_transform('(he).*(ri)', 'henrik')";
    // let mut stmt = database.prepare(query).unwrap();
    // let mut rows = stmt.query([]).unwrap();
    // let row = rows.next().unwrap().unwrap();
    // let result: String = row.get(0).unwrap();
    // assert_eq!(result, "heri");
    // }

    // #[test]
    // fn custom_functions_regexp_transform_with_capture_test() {
    //     let database = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
    //     let table_name = database.get_current_table_name().unwrap();
    //     let header = database.get_headers(&table_name).unwrap()[1].clone();
    //     let pattern = "(e.).*(r)";
    //     let transformation = "${1}x${2}";
    //
    //     let query = regexping::build_regex_with_capture_group_transform_query(
    //         &header,
    //         pattern,
    //         transformation,
    //         &table_name,
    //     )
    //         .unwrap();
    //
    //     database.execute_batch(&query).unwrap();
    //
    //     let result: String = database
    //         .connection
    //         .query_row(
    //             "SELECT derivedfirstname FROM `data` WHERE id = 1 ORDER BY rowid ASC",
    //             [],
    //             |row| row.get(0),
    //         )
    //         .unwrap();
    //     assert_eq!(result, "rxen");
    // }
    //
    // #[test]
    // fn my_benching_stuff() {
    //     let before = Instant::now();
    //     let database = Database::try_from(vec![PathBuf::from("assets/customers-1000000.csv")]).unwrap();
    //     let table_name = database.get_current_table_name().unwrap();
    //     let header = database.get_headers(&table_name).unwrap()[2].clone();
    //     let pattern = "n.*";
    //
    //     let query = regexping::build_regex_filter_query(&header, pattern, &table_name).unwrap();
    //
    //     database.execute_batch(&query).unwrap();
    //
    //     let table_name = database.get_table_names().unwrap()[1].clone();
    //     let result: u32 = database
    //         .connection
    //         .query_row(
    //             &format!("SELECT COUNT(*) FROM `{table_name}`;"),
    //             [],
    //             |row| row.get(0),
    //         )
    //         .unwrap();
    //     println!("result: {}", result);
    //
    //     println!("Elapsed time: {:.2?}", before.elapsed());
    //     assert_ne!(true, true);
    // }
    //
    // #[test]
    // fn my_benching_no_capture() {
    //     let before = Instant::now();
    //     let database = Database::try_from(vec![PathBuf::from("assets/customers-1000000.csv")]).unwrap();
    //     let table_name = database.get_current_table_name().unwrap();
    //     let header = database.get_headers(&table_name).unwrap()[2].clone();
    //     let pattern = "n.*";
    //
    //     let query =
    //         regexping::build_regex_no_capture_group_transform_query(&header, pattern, &table_name)
    //             .unwrap();
    //
    //     // let sql =
    //     //     "UPDATE TABLE `cRegexFiltered` AS SELECT * FROM `c` WHERE regexp('n.*', `firstname`);";
    //
    //     // let sql = "ALTER TABLE `c` ADD COLUMN `derivedfirstname` TEXT;\n";
    //     // database.execute(sql, []).unwrap();
    //     // let sql = "UPDATE `c` \
    //     //         SET 'derivedfirstname' = regexp_transform_no_capture_group('n.*', `firstname`) \
    //     //         WHERE id IN (SELECT id FROM `c` WHERE `firstname` REGEXP 'n.*');\n";
    //     // database.execute(sql, []).unwrap();
    //     // let names = database.get_table_names().unwrap();
    //     // dbg!(names);
    //
    //     // let query = "SELECT * FROM `c` ORDER BY rowid ASC LIMIT 10;".to_string();
    //     // let mut stmt = database.prepare(&query).unwrap();
    //     // let mut rows = stmt.query([]).unwrap();
    //
    //     // while let Some(row) = rows.next().unwrap_or(None) {
    //     //     let datarow: DataRow = DataRow::from(row);
    //     //     println!("{:?}", datarow);
    //     // }
    //     database.execute_batch(&query).unwrap();
    //
    //     // let table_name = database.get_table_names().unwrap()[1].clone();
    //     let result: u32 = database
    //         .connection
    //         .query_row("SELECT COUNT(*) FROM `customers-1000000`;", [], |row| {
    //             row.get(0)
    //         })
    //         .unwrap();
    //     println!("result: {}", result);
    //
    //     println!("Elapsed time: {:.2?}", before.elapsed());
    //     assert_ne!(true, true);
    // }
}
