use std::{error::Error, fs::File, path::Path};

use csv::{Reader, StringRecord};
use rusqlite::Connection;

use super::database::Database;

pub(crate) fn database_from_csv(
    path: &Path,
    connection: Connection,
) -> crate::error::AppResult<Database> {
    let mut csv = csv::Reader::from_path(path)?;
    let table_name = Database::get_table_name(path).unwrap();
    let table_names = vec![table_name.to_string()];
    let mut queries = String::new();
    let query = build_create_table_query(&mut csv, table_name).unwrap();
    queries.push_str(&query);
    connection.execute_batch(&queries)?;
    queries.clear();
    let columns = get_headers_for_query(&mut csv, table_name).unwrap();
    let mut database = Database::new(connection)?;
    let limit = 10000;
    let mut i = 0;

    let records = csv.records();
    // let capacity = if len < limit { len + 3 } else { limit + 3 };
    let mut items = Vec::with_capacity(limit);
    // TODO escape single quotes properly.
    for record in records {
        let record = record?;
        let result = build_value_query(&record);
        items.push(result);
        i += 1;
        if i == limit {
            i = 0;
            queries.clear();
            queries = format!(
                "INSERT INTO '{}' ({}) VALUES \n{};",
                table_name,
                columns,
                items.join(",\n")
            );
            database.connection.execute_batch(&queries)?;
            items.clear();
        }
    }
    if !items.is_empty() {
        queries.clear();
        queries = format!(
            "INSERT INTO '{}' ({}) VALUES {};",
            table_name,
            columns,
            items.join(",")
        );
        database.connection.execute_batch(&queries)?;
    }
    let query =
        "SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;".to_string();
    let table_idx: u16 = database
        .connection
        .query_row(&query, [], |row| row.get(0))?;
    database.current_table_idx = table_idx;

    let query = format!(
        "SELECT * FROM `{}` ORDER BY rowid ASC LIMIT 50 OFFSET 0;",
        table_name,
    );
    Ok(database)
}

fn build_value_query(record: &StringRecord) -> String {
    let row = record
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "XXXXXXXXXXX")))
        .collect::<Vec<String>>()
        .join(",");
    format!("({})", row)
}

pub(crate) fn database_from_sqlite(connection: Connection) -> crate::error::AppResult<Database> {
    let database = Database::new(connection)?;

    Ok(database)
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

pub(crate) fn get_headers_for_query(
    csv: &mut Reader<File>,
    table_name: &str,
) -> Result<String, Box<dyn Error>> {
    let headers = csv.headers()?;
    let columns: String = headers
        .iter()
        .map(|header| format!("'{}'", header))
        .collect::<Vec<String>>()
        .join(", ");
    Ok(columns)
}
