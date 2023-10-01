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
use rusqlite::Result;
use std::sync::Arc;

use crate::error::AppError;
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug)]
pub struct Database {
    pub connection: Connection,
    pub(crate) table_names: HashSet<String>,
    pub current_header_idx: u32,
    pub order_column: String,
    pub is_asc_order: bool,
    pub state: TableState,
}

impl Database {
    pub fn get_current_header(&self) -> String {
        self.get(0, "data").0[self.current_header_idx as usize].clone()
    }
    pub fn get(&self, limit: i32, table_name: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let mut sheet = vec![];
        let ordering = if self.is_asc_order { "ASC" } else { "DESC" };
        let query = format!(
            "SELECT * FROM {} ORDER BY {} {} LIMIT {};",
            table_name, self.order_column, ordering, limit
        );
        let mut stmt = self.connection.prepare(&query).unwrap();
        let cols = stmt
            .column_names()
            .iter()
            .map(<&str>::to_string)
            .collect::<Vec<String>>();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
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
        (cols, sheet)
    }

    pub fn derive_column(
        &self,
        column_name: String,
        fun: fn(String) -> String,
    ) -> Result<(), Box<dyn Error>> {
        // create a new column in the table. The new value for each row is the value string value of column name after running fun function on it.
        let derived_column_name = format!("derived-{}", column_name);

        let create_column_query = format!(
            "ALTER TABLE data ADD COLUMN '{}' TEXT;",
            derived_column_name
        );
        self.connection.execute(&create_column_query, [])?;

        // for each row in the table, run fun on the value of column name and insert the result into the new column
        let query = format!("SELECT id, `{}` FROM data", column_name);
        let mut binding = self.connection.prepare(&query)?;
        let mut rows = binding.query([])?;

        // TODO use a transaction
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let value: String = row.get(1)?;
            let derived_value = fun(value);
            let update_query = format!(
                "UPDATE {} SET '{}' = ? WHERE id = ?",
                "data", derived_column_name
            );
            self.connection
                .execute(&update_query, params![derived_value, id])?;
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
            // Database::new(Connection::open("db.sqlite")?, table_names)
            Database::new(Connection::open_in_memory()?, table_names)
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
            .map(|header| format!("'{}'", header))
            .collect::<Vec<String>>()
            .join(", ");

        let headers_string: String = headers
            .iter()
            .map(|header| format!("\n\t{} TEXT", header))
            .collect::<Vec<String>>()
            .join(",");
        let query = format!(
            "CREATE TABLE IF NOT EXISTS {} (\n\tid INTEGER PRIMARY KEY, {}\n);",
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
    fn count_headers(&self) -> u32 {
        let mut stmt = self
            .connection
            .prepare("SELECT COUNT(*) FROM PRAGMA_TABLE_INFO('data')")
            .unwrap();

        let r: u32 = stmt.query_row([], |row| row.get(0)).unwrap();
        r
    }
    pub(crate) fn next_header(&mut self) {
        let i = self.current_header_idx + 1;
        if i >= self.count_headers() {
            self.current_header_idx = 0;
        } else {
            self.current_header_idx = i;
        }
    }
    pub(crate) fn previous_header(&mut self) {
        let i = self.current_header_idx;
        if i == 0 {
            self.current_header_idx = self.count_headers() - 1;
        } else {
            self.current_header_idx = i - 1;
        }
    }

    pub fn next_row(&mut self, height: u16) {
        let i = match self.state.selected() {
            Some(i) if i <= height as usize => i + 1,
            _ => 0,
        };
        self.state.select(Some(i));
    }

    pub fn previous_row(&mut self, height: u16) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    height as usize - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
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
        database.next_header();
        assert_eq!(1, database.current_header_idx);
        database.next_header();
        assert_eq!(2, database.current_header_idx);
        database.next_header();
        assert_eq!(3, database.current_header_idx);
        database.next_header();
        assert_eq!(0, database.current_header_idx);
    }

    #[test]
    fn dec_header() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        database.previous_header();
        assert_eq!(3, database.current_header_idx);
        database.previous_header();
        assert_eq!(2, database.current_header_idx);
        database.previous_header();
        assert_eq!(1, database.current_header_idx);
        database.previous_header();
        assert_eq!(0, database.current_header_idx);
    }

    #[test]
    fn derive_column_test() {
        let mut database = Database::try_from(Path::new("assets/data.csv")).unwrap();
        let col = "firstname";
        let fun = |s| format!("{}-changed", s);
        database.derive_column(col.to_string(), fun).unwrap();
        let first = database.get(1, "data").1[0][4].clone();
        assert_eq!(first, "henrik-changed");
        assert_eq!(database.count_headers(), 5);
    }
}
