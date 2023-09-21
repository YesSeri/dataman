// read csv file to in memory database

use csv::Reader;
use rusqlite::Connection;
use std::error::Error;
use std::fmt::format;
use std::path::Path;

use crate::db;

#[derive(Debug)]
struct Item {
    first: String,
    last: String,
    age: String,
}

pub fn open_connection(path: &Path) -> Result<(), Box<dyn Error>> {
    // create table with headers as columns
    let conn = Connection::open_in_memory()?;

    let _ = std::fs::remove_file("db.sqlite");
    let conn = Connection::open("db.sqlite")?;

    create_table_from_csv(path, &conn);
    add_table_data_from_csv(path, &conn);

    Ok(())
}
fn create_table_from_csv(path: &Path, conn: &Connection) -> Result<(), Box<dyn Error>> {
    let mut csv = csv::Reader::from_path(path)?;
    let headers = csv.headers()?;
    let table_name = get_table_name(path).unwrap_or("default_table");
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
    let create_table_query = format!(
        "CREATE TABLE IF NOT EXISTS {} ({}\n);",
        table_name, headers_string
    );
    conn.execute(&create_table_query, ())?;
    Ok(())
}
fn add_table_data_from_csv(path: &Path, conn: &Connection) -> Result<(), Box<dyn Error>> {
    let mut csv = csv::Reader::from_path(path)?;

    let table_name = get_table_name(path).unwrap_or("default_table");
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
                "({})",
                row.iter()
                    .map(|el| format!("'{}'\n", el))
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
    conn.execute(&query, ())?;
    Ok(())
}
fn get_table_name(file: &Path) -> Option<&str> {
    file.file_stem()?.to_str()
}

// tests

#[cfg(test)]
mod tests {
    use super::*;
}
