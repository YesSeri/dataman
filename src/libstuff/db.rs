// read csv file to in memory database

use csv::Reader;
use ratatui::widgets::TableState;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection};
use std::cmp;
use std::collections::HashSet;
use std::error::Error;
use std::fs::File;
use std::path::Path;

use regex::Regex;
use rusqlite::functions::FunctionFlags;
use std::sync::Arc;

use crate::error::{AppError, AppResult};
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug)]
pub struct Database {
    pub connection: Connection,
    pub(crate) table_names: HashSet<String>,
    pub current_header_idx: u32,
    pub order_column: String,
    pub is_asc_order: bool,
    pub state: TableState,
    row_idx: usize,
}

impl Database {
    pub fn get_current_header(&self) -> AppResult<String> {
        let binding = "default table name".to_string();
        let table_name = self.table_names.iter().next().unwrap_or(&binding);
        Ok(self.get(0, 0, table_name)?.0[self.current_header_idx as usize].clone())
    }
    pub fn get(
        &self,
        limit: i32,
        offset: i32,
        table_name: &str,
    ) -> AppResult<(Vec<String>, Vec<Vec<String>>)> {
        let mut sheet = vec![];
        let ordering = if self.is_asc_order { "ASC" } else { "DESC" };
        let query = format!(
            "SELECT * FROM `{}` ORDER BY `{}` {} LIMIT {} OFFSET {};",
            table_name, self.order_column, ordering, limit, offset
        );
        let mut stmt = self.connection.prepare(&query).unwrap();
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
        let table_name = self.get_first_table();
        let query = format!("SELECT `{}` FROM `{}` WHERE id = ?;", header, table_name);
        let mut stmt = self.connection.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        let row = rows.next()?.unwrap();
        let cell = row.get(0)?;
        Ok(cell)
    }

    pub fn derive_column(&self, column_name: String, fun: fn(String) -> String) -> AppResult<()> {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        let derived_column_name = format!("derived-{}", column_name);

        let table_name = self.table_names.iter().next().unwrap();
        let create_column_query = format!(
            "ALTER TABLE `{table_name}` ADD COLUMN '{}' TEXT;",
            derived_column_name
        );
        self.connection.execute(&create_column_query, [])?;

        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let query = format!("SELECT id, `{}` FROM `{table_name}`", column_name);
        let mut binding = self.connection.prepare(&query)?;
        let mut rows = binding.query([])?;

        // TODO use a transaction
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value);
            let table_name = self.table_names.iter().next().unwrap();
            let update_query = format!(
                "UPDATE `{}` SET '{}' = ? WHERE id = ?",
                table_name, derived_column_name
            );
            self.connection
                .execute(&update_query, params![derived_value, id])?;
        }

        Ok(())
    }

    pub(crate) fn get_current_id(&self) -> AppResult<i32> {
        let i = self.state.selected().unwrap_or(0) + 1;
        let query = format!(
            "SELECT rowid FROM `{}` WHERE rowid = ?",
            self.get_first_table()
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
    // TODO 1. add ability to take input.
    // TODO 2. user sql query
    // TODO 3. user regex fn
}

impl Database {
    pub(crate) fn insert_table_name(&mut self, table_name: String) {
        self.table_names.insert(table_name);
    }
}

impl TryFrom<&Path> for Database {
    type Error = Box<dyn Error>;

    fn try_from(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut csv = csv::Reader::from_path(path)?;
        let table_name = Database::get_table_name(path).unwrap();

        let table_names = HashSet::from([table_name.to_string()]);
        let database: Database = if cfg!(debug_assertions) {
            let _ = std::fs::remove_file("db.sqlite");
            Database::new(Connection::open("db.sqlite")?, table_names)
            // Database::new(Connection::open_in_memory()?, table_names)
        } else {
            Database::new(Connection::open_in_memory()?, table_names)
        };
        let funs = vec![Self::build_create_table_query, Self::build_add_data_query];
        for fun in funs {
            let query = fun(&mut csv, table_name)?;
            database.connection.execute(&query, ())?;
        }
        Ok(database)
    }
}

impl Database {
    pub fn new(connection: Connection, table_names: HashSet<String>) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Database {
            connection,
            table_names,
            current_header_idx: 0,
            order_column: "id".to_string(),
            is_asc_order: true,
            row_idx: 0,
            state,
        }
    }
    fn build_create_table_query(
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

    fn build_add_data_query(
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

    fn get_table_name(file: &Path) -> Option<&str> {
        file.file_stem()?.to_str()
    }
    pub fn count_headers(&self) -> AppResult<u32> {
        let table_name = self
            .table_names
            .iter()
            .next()
            .ok_or(AppError::Sqlite(rusqlite::Error::InvalidQuery))?; // TODO handle error
        let query = format!("SELECT COUNT(*) FROM PRAGMA_TABLE_INFO('{}')", table_name);
        let mut stmt = self.connection.prepare(&query)?;

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
    fn get_first_table(&self) -> String {
        return self.table_names.iter().next().unwrap().to_string();
    }
    pub fn update_cell(&mut self, header: &str, id: i32, content: &str) -> AppResult<()> {
        let table_name = self.get_first_table();
        let update_query = format!("UPDATE `{}` SET '{}' = ? WHERE id = ?;", table_name, header);
        self.connection
            .execute(&update_query, params![content, id])?;
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
        let database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let col = "firstname";
        let fun = |s| format!("{}-changed", s);
        database.derive_column(col.to_string(), fun).unwrap();
        let first = database.get(1, 0, "data").unwrap().1[0][4].clone();
        assert_eq!(first, "henrik-changed");
        let n = database.count_headers().unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn update_cell() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        database.update_cell("firstname", 1, "hank").unwrap();
        let first = database.get(1, 0, "data").unwrap().1[0][1].clone();
        assert_eq!(first, "hank");
    }
    #[test]
    fn test_offset() {
        let database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let (_, first) = database.get(1, 0, "data").unwrap();
        assert_eq!("henrik", first[0][1]);
        let (_, second) = database.get(1, 1, "data").unwrap();
        assert_eq!("john", second[0][1]);
    }
}
