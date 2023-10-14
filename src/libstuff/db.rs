// read csv file to in memory database

use csv::Reader;
use ratatui::widgets::TableState;
use regex::Regex;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, Statement};
use std::error::Error;
use std::fs::File;
use std::hash::Hash;
use std::path::Path;
use std::process::id;

use crossterm::event::KeyCode::F;
use crossterm::ExecutableCommand;
use rusqlite::functions::FunctionFlags;
use std::sync::Arc;

use crate::error::{log, AppError, AppResult};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug)]
pub struct Database {
    pub connection: Connection,
    pub current_header_idx: u32,
    pub order_column: String,
    pub is_asc_order: bool,
    pub state: TableState,
    pub(super) current_table_idx: usize,
    row_idx: usize,
}

impl Database {
    pub fn get_current_header(&self) -> AppResult<String> {
        let binding = vec!["default table name".to_string()];
        let table_name = self.get_current_table_name()?;
        Ok(self.get(0, 0, table_name)?.0[self.current_header_idx as usize].clone())
    }
    pub fn get(
        &self,
        limit: i32,
        offset: i32,
        table_name: String,
    ) -> AppResult<(Vec<String>, Vec<Vec<String>>)> {
        let mut sheet = vec![];
        let ordering = if self.is_asc_order { "ASC" } else { "DESC" };
        let query = format!(
            "SELECT * FROM `{}` ORDER BY `{}` {} LIMIT {} OFFSET {};",
            table_name, self.order_column, ordering, limit, offset
        );
        let mut stmt = self.prepare(&query).unwrap();
        let cols = stmt
            .column_names()
            .iter()
            .map(<&str>::to_string)
            .collect::<Vec<String>>();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let data = cols
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    let field = row.get_ref(i).unwrap();
                    match field {
                        ValueRef::Null => "NULL".to_string(),
                        ValueRef::Integer(cell) => cell.to_string(),
                        ValueRef::Real(_) => unimplemented!("real"),
                        ValueRef::Text(cell) => {
                            let cell = std::str::from_utf8(cell).unwrap();
                            cell.to_string()
                        }
                        ValueRef::Blob(_) => unimplemented!("blob"),
                    }
                })
                .collect::<Vec<String>>();
            sheet.push(data);
        }
        Ok((cols, sheet))
    }
    pub fn get_cell(&self, id: i32, header: &str) -> AppResult<String> {
        let table_name = self.get_current_table_name()?;
        let query = format!("SELECT `{}` FROM `{}` WHERE id = ?;", header, table_name);
        let mut stmt = self.prepare(&query)?;
        if cfg!(debug_assertions) {
            log(format!("id: {}", id));
        }
        let mut rows = stmt.query(params![id])?;
        let row = rows.next()?.unwrap();
        let cell = row.get(0)?;

        Ok(cell)
    }
    pub fn prepare(&self, sql: &str) -> rusqlite::Result<Statement> {
        if cfg!(debug_assertions) {
            log(sql.to_string());
        }
        self.connection.prepare(sql)
    }
    pub fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> AppResult<()> {
        if cfg!(debug_assertions) {
            log(sql.to_string());
        }
        self.connection.execute(sql, params)?;
        Ok(())
    }

    pub fn execute_batch(&self, sql: &str) -> AppResult<()> {
        let query = &format!("BEGIN TRANSACTION;{}COMMIT;", sql);
        if cfg!(debug_assertions) {
            log(query.to_string());
        }

        match self.connection.execute_batch(query) {
            Ok(_) => Ok(()),
            Err(err) => {
                let _ = self.execute("ROLLBACK;", []);
                AppResult::Err(AppError::Sqlite(err))
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
        // TODO use a transaction
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value).unwrap_or("NULL".to_string());
            let table_name = self.get_table_names()?[0].clone();
            let update_query = format!(
                "UPDATE `{table_name}` SET '{derived_column_name}' = '{derived_value}' WHERE id = '{id}';\n",
            );
            transaction.push_str(&update_query);
        }
        self.execute_batch(&transaction)?;
        Ok(())
    }

    pub(crate) fn get_current_id(&self) -> AppResult<i32> {
        let i = self.state.selected().unwrap_or(0);
        let query = format!(
            "SELECT rowid FROM `{}` LIMIT 1 OFFSET {};",
            self.get_current_table_name()?,
            i
        );
        let a: i32 = self
            .connection
            .query_row(&query, [i], |row| row.get(0))
            .unwrap();
        Ok(a)
    }

    pub(crate) fn sort(&mut self) -> AppResult<()> {
        // sort by current header
        let header = self.get_current_header()?;
        self.state.select(Some(0));
        if self.order_column == header {
            self.is_asc_order = !self.is_asc_order;
        } else {
            self.is_asc_order = true;
            self.order_column = header;
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
    pub fn next_table(&mut self) -> AppResult<()> {
        let query = format!(
            "SELECT rowid FROM sqlite_master WHERE type='table' AND rowid > {} ORDER BY rowid;",
            self.current_table_idx
        );
        self.current_table_idx = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(())
    }
    pub fn get_current_table_name(&self) -> AppResult<String> {
        // let query = format!("SELECT rowid FROM sqlite_master WHERE type='table';");
        // let mut stmt = self.prepare(&query)?;
        // let mut rows = stmt.query([])?;
        // while let Some(r) = rows.next()? {
        //     let id: usize = r.get(0)?;
        //     log("name: {}",  id);
        // }
        let query = format!(
            "SELECT name FROM sqlite_master WHERE type='table' AND rowid={};",
            self.current_table_idx
        );
        let table_name = self.connection.query_row(&query, [], |row| row.get(0))?;
        Ok(table_name)
    }
    pub fn regex_filter(&mut self, header: &str, pattern: &str) -> AppResult<()> {
        // create new table with filter applied using create table as sqlite statement.
        regex::Regex::new(pattern)?;
        let table_name = self.get_current_table_name()?;
        let select_query =
            format!("SELECT * FROM `{table_name}` WHERE `{header}` REGEXP '{pattern}'");
        let create_table_query =
            format!("CREATE TABLE `{table_name}RegexFiltered` AS {select_query};");

        self.execute(&create_table_query, [])?;
        self.next_table()?;
        Ok(())
    }

    pub(crate) fn regex(&self, pattern: &str, column_name: String) -> AppResult<()> {
        let fun = |s: String| {
            let re = Regex::new(pattern).map_err(AppError::Regex).ok()?;
            let first_match: AppResult<_> = re.captures_iter(&s).next().ok_or(AppError::Other);
            eprintln!("first match: {:?}", first_match);
            first_match
                .ok()
                .map(|m| m.get(0))?
                .map(|c| c.as_str().to_string())
        };
        self.derive_column(column_name, fun)
    }
    // TODO 1. add ability to take input.
    // TODO 2. user sql query
    // TODO 3. user regex fn
}

impl TryFrom<&Path> for Database {
    type Error = Box<dyn Error>;

    fn try_from(path: &Path) -> Result<Self, Box<dyn Error>> {
        let extension = path.extension().unwrap();
        match extension {
            os_str if os_str == "csv" => {
                let database = super::converter::database_from_csv(path)?;
                Ok(database)
                // let mut csv = csv::Reader::from_path(path)?;
                // let table_name = Database::get_table_name(path).unwrap();
                // let table_names = vec![table_name.to_string()];
                // let mut database: Database = Database::new(table_names);
                // let funs = vec![Self::build_create_table_query, Self::build_add_data_query];
                // for fun in funs {
                //     let query = fun(&mut csv, table_name)?;
                //     database.execute(&query, ())?;
                // }

                // let query =
                //     "SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;"
                //         .to_string();
                // let table_idx: usize = database
                //     .connection
                //     .query_row(&query, [], |row| row.get(0))
                //     .unwrap();
                // database.current_table_idx = table_idx;
                // Ok(database)
            }
            os_str if os_str == "sqlite" => {
                let database = super::converter::database_from_sqlite(path)?;
                todo!();
                // Ok(database)
                // Self::try_from_sqlite(path)
                unimplemented!("sqlite");
            }
            _ => panic!("Unsupported file format"),
        }
    }
}

impl Database {
    pub fn new(table_names: Vec<String>) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        let connection = if cfg!(debug_assertions) {
            let _ = std::fs::remove_file("db.sqlite");
            Connection::open("db.sqlite").unwrap()
            // Connection::open_in_memory().unwrap()
        } else {
            Connection::open_in_memory().unwrap()
        };

        let database = Database {
            connection,
            current_header_idx: 0,
            order_column: "id".to_string(),
            is_asc_order: true,
            row_idx: 0,
            current_table_idx: 0,
            state,
        };
        database.add_custom_functions().unwrap_or_else(|e| {
            log(format!("Error adding custom functions, e.g. REGEXP: {}", e));
        });
        database
    }
    fn add_custom_functions(&self) -> rusqlite::Result<()> {
        self.connection.create_scalar_function(
            "regexp",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let regex = ctx.get::<String>(0)?;
                let text = ctx.get::<String>(1)?;
                let result = regex::Regex::new(&regex).unwrap().is_match(&text);
                Ok(result)
            },
        )
    }
    pub(crate) fn build_create_table_query(
        csv: &mut Reader<File>,
        table_name: &str,
    ) -> Result<String, Box<dyn Error>> {
        let headers = csv.headers()?;
        let columns: String = headers
            .iter()
            .map(|header| format!("`{}`", header))
            .collect::<Vec<String>>()
            .join(", ");

        let headers_string: String = headers
            .iter()
            .map(|header| format!("\n\t`{}` TEXT", header))
            .collect::<Vec<String>>()
            .join(",");
        let query = format!(
            "CREATE TABLE IF NOT EXISTS '{}' (\n\tid INTEGER PRIMARY KEY, {}\n);",
            table_name, headers_string
        );
        Ok(query)
    }

    pub(crate) fn build_add_data_query(
        csv: &mut Reader<File>,
        table_name: &str,
    ) -> Result<String, Box<dyn Error>> {
        let headers = csv.headers()?;
        let columns: String = headers
            .iter()
            .map(|header| format!("'{}'", header))
            .collect::<Vec<String>>()
            .join(", ");

        let values = csv
            .records()
            .map(|row| {
                let row = row.unwrap();
                format!(
                    "\n\t({})",
                    row.iter()
                        .map(|el| format!("'{}'", el))
                        .collect::<Vec<String>>()
                        .join(", ")
                )
            })
            .collect::<Vec<String>>()
            .join(",");

        let query = format!(
            "INSERT INTO '{}' ({}) VALUES {};",
            table_name, columns, values
        );

        Ok(query)
    }

    pub(crate) fn get_table_name(file: &Path) -> Option<&str> {
        file.file_stem()?.to_str()
    }
    pub fn count_headers(&self) -> AppResult<u32> {
        let table_name = self.get_current_table_name()?;
        let query = format!("SELECT COUNT(*) FROM PRAGMA_TABLE_INFO('{}')", table_name);
        let mut stmt = self.prepare(&query)?;

        let r = stmt.query_row([], |row| row.get(0));
        r.map_err(AppError::Sqlite)
    }
    pub(crate) fn next_header(&mut self) -> AppResult<()> {
        self.current_header_idx = (self.current_header_idx + 1) % self.count_headers()?;
        Ok(())
    }
    pub(crate) fn previous_header(&mut self) -> AppResult<()> {
        if self.current_header_idx == 0 {
            self.current_header_idx = self.count_headers()?;
        };
        self.current_header_idx -= 1;
        Ok(())
    }

    pub fn next_row(&mut self, height: u16) {
        let i = match self.state.selected() {
            Some(i) if i <= height as usize => i + 1,
            _ => 0,
        };
        // let query = format!("SELECT id FROM '{}' LIMIT 1 OFFSET {}", self.get_first_table(), self.state.offset());
        self.state.select(Some(i));
    }

    pub fn previous_row(&mut self, height: u16) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    height as usize + 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }
    pub fn update_cell(&self, header: &str, id: i32, content: &str) -> AppResult<()> {
        let table_name = self.get_current_table_name()?;
        let update_query = format!("UPDATE `{}` SET '{}' = ? WHERE id = ?;", table_name, header);
        self.execute(&update_query, params![content, id])?;
        Ok(())
    }
}
// tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_number_of_headers_test() {
        let database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let number_of_headers = database.count_headers();
    }

    #[test]
    fn inc_header() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        database.next_header().unwrap();
        assert_eq!(1, database.current_header_idx);
        database.next_header().unwrap();
        assert_eq!(2, database.current_header_idx);
        database.next_header().unwrap();
        assert_eq!(3, database.current_header_idx);
        database.next_header().unwrap();
        assert_eq!(0, database.current_header_idx);
    }

    #[test]
    fn dec_header() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        database.previous_header().unwrap();
        assert_eq!(3, database.current_header_idx);
        database.previous_header().unwrap();
        assert_eq!(2, database.current_header_idx);
        database.previous_header().unwrap();
        assert_eq!(1, database.current_header_idx);
        database.previous_header().unwrap();
        assert_eq!(0, database.current_header_idx);
    }

    #[test]
    fn derive_column_test() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let col = "firstname";
        let fun = |s| Some(format!("{}-changed", s));
        database.derive_column(col.to_string(), fun).unwrap();
        let first = database.get(1, 0, "data".to_string()).unwrap().1[0][4].clone();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers().unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn update_cell() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        database.update_cell("firstname", 1, "hank").unwrap();
        let first = database.get(1, 0, "data".to_string()).unwrap().1[0][1].clone();
        assert_eq!(first, "hank");
    }

    #[test]
    fn test_offset() {
        let database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let (_, first) = database.get(1, 0, "data".to_string()).unwrap();
        assert_eq!("henrik", first[0][1]);
        let (_, second) = database.get(1, 1, "data".to_string()).unwrap();
        assert_eq!("john", second[0][1]);
    }

    #[test]
    fn get_table_names_test() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let table_names = database.get_table_names().unwrap();
    }

    #[test]
    fn get_current_table_name_test() {
        // let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        // let table_name = database.get_current_table_name().unwrap();
        // assert_eq!(table_name, "data".to_string());
    }
}
