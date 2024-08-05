use std::path::PathBuf;
use std::{error::Error, fs::File, path, path::Path};

use csv::{Reader, StringRecord, Writer};
use rusqlite::{Connection, Rows};
use serde::Serialize;

use crate::app_error_other;
use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use super::database::Database;

pub(crate) fn database_from_csv(path: PathBuf, connection: Connection) -> AppResult<Database> {
    insert_csv_data_database(path, &connection)?;
    let mut database = Database::new(connection)?;
    let query =
        r#"SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;"#.to_string();
    let table_idx: u16 = database
        .connection
        .query_row(&query, [], |row| row.get(0))?;
    database.current_table_idx = table_idx;

    Ok(database)
}

fn value_query(record: &StringRecord) -> String {
    let row = record
        .iter()
        .map(|s| {
            if s.is_empty() {
                "NULL".to_string()
            } else {
                format!("'{}'", s.replace('\'', "''"))
            }
        })
        .collect::<Vec<String>>()
        .join(",");
    format!("({})", row)
}

pub(crate) fn database_from_sqlite(connection: Connection) -> AppResult<Database> {
    let database = Database::new(connection)?;

    Ok(database)
}

pub(crate) fn create_table_query(
    csv: &mut Reader<File>,
    table_name: &str,
) -> Result<String, Box<dyn Error>> {
    let headers = csv.headers()?;
    let columns: String = headers
        .iter()
        .map(|header| format!(r#""{}""#, header))
        .collect::<Vec<String>>()
        .join(", ");

    let headers_string: String = headers
        .iter()
        .map(|header| format!(r#""{}" TEXT"#, header))
        .collect::<Vec<String>>()
        .join(",");
    log::info!("Creating table with headers: {}", headers_string);
    let query = format!(
        r#"CREATE TABLE IF NOT EXISTS '{}'
	(id INTEGER PRIMARY KEY, {})
	;"#,
        table_name, headers_string
    );
    log::info!("Query: {}", query);
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

pub(crate) fn sqlite_to_out(connection: &Connection, path: PathBuf) -> AppResult<()> {
    let mut stmt = connection
        .prepare(r#"SELECT "name" FROM sqlite_master WHERE type='table' AND name != 'table_of_tables' ORDER BY "name";"#)?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut wtr = csv::Writer::from_path(path)?;
    for table in tables {
        let query = &format!(r#"SELECT * FROM "{}";"#, table);
        let mut stmt = connection.prepare(query)?;
        let headers = stmt
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<String>>();
        let rows = stmt.query_map([], |row| {
            let mut items = Vec::new();

            for i in 0..headers.len() {
                let header = headers.get(i).unwrap();
                dbg!(header);
                if header == "id" {
                    continue;
                }
                let item = DataItem::from(row.get_ref(i).unwrap());
                items.push(item);
            }
            Ok(items)
        })?;
        wtr.write_record(headers.iter().filter(|&el| el != "id").clone())
            .unwrap();
        for row in rows {
            let row = row?;
            let row: Vec<Option<DataItem>> = row
                .into_iter()
                .map(|el| match el {
                    DataItem::Null => None,
                    o => Some(o),
                })
                .collect();
            wtr.serialize(row.clone()).unwrap();
        }
    }

    Ok(())
}

const LIMIT: usize = 10000;

pub(crate) fn insert_csv_data_database(
    path: PathBuf,
    connection: &Connection,
) -> Result<(), AppError> {
    let mut csv = csv::ReaderBuilder::new().from_path(&path)?;
    let table_name =
        Database::get_table_name(path).ok_or(app_error_other!("could not get table name."))?;
    let table_names = [table_name.to_string()];
    let mut queries = String::new();
    let query = create_table_query(&mut csv, &table_name).unwrap();
    queries.push_str(&query);
    connection.execute_batch(&queries)?;
    queries.clear();
    let columns = get_headers_for_query(&mut csv, &table_name).unwrap();
    let mut i = 0;

    let records = csv.records();
    let mut items: Vec<String> = Vec::with_capacity(LIMIT);
    for record in records {
        i += 1;
        create_insert_stmt_record(record?, &mut items);
        if should_batch_execute(i) {
            i = 0;
            batch_exec_and_clear(&mut queries, &table_name, &columns, &mut items, connection)?;
        }
    }
    if !items.is_empty() {
        batch_exec_and_clear(&mut queries, &table_name, &columns, &mut items, connection)?;
    }

    Ok(())
}
fn should_batch_execute(i: usize) -> bool {
    i == LIMIT
}
fn batch_exec_and_clear(
    queries: &mut String,
    table_name: &str,
    columns: &str,
    items: &mut Vec<String>,
    connection: &Connection,
) -> AppResult<()> {
    queries.clear();
    *queries = format!(
        r#"INSERT INTO '{}' ({}) VALUES {};"#,
        table_name,
        columns,
        items.join(",\n")
    );
    connection.execute_batch(queries)?;
    items.clear();
    Ok(())
}

fn create_insert_stmt_record(record: StringRecord, items: &mut Vec<String>) {
    let result = value_query(&record);
    items.push(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::{assert_eq, println};

    #[test]
    fn write_db_to_out_test() {
        let mut database1 = Database::try_from(vec![PathBuf::from("assets/data.sqlite")]).unwrap();
        sqlite_to_out(&database1.connection, PathBuf::from("assets/out-data.csv")).unwrap();
        let mut database2 = Database::try_from(vec![PathBuf::from("assets/out-data.csv")]).unwrap();
        let first_row_db1 = database1.get(1, 0, "data".to_string()).unwrap().1;
        let first_row_db2 = database2.get(1, 0, "out-data".to_string()).unwrap().1;

        for (i, item) in first_row_db1.iter().enumerate() {
            assert_eq!(item, first_row_db2.get(i).unwrap());
        }
    }
}
