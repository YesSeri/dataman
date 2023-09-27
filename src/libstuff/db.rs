// read csv file to in memory database

use csv::Reader;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use std::collections::HashSet;
use std::error::Error;
use std::fs::File;
use std::path::Path;

#[derive(Debug)]
pub struct Database {
    pub connection: Connection,
    pub(crate) table_names: HashSet<String>,
}

impl Database {
    pub fn get(&self, limit: i32, table_name: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let mut sheet = vec![];
        let query = format!("SELECT * FROM {table_name} LIMIT {limit};");
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
                        ValueRef::Null => unimplemented!("null"),
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
        Database {
            connection,
            table_names,
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
}
// tests

#[cfg(test)]
mod tests {
    use super::*;
}
