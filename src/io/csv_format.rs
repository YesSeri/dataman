use std::path::PathBuf;
use std::{error::Error, fs::File, path, path::Path};

use csv::{Reader, StringRecord, Writer};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Rows};
use serde::Serialize;

use crate::app_error_other;
use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use crate::model::database::{Database, INTERNAL_PREFIX};

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) fn database_from_csv(path: PathBuf, connection: Connection) -> AppResult<Database> {
    insert_csv_data_database(path, &connection)?;
    Database::new(connection)
}

fn value_query(record: &StringRecord) -> String {
    let row = record
        .iter()
        .map(|s| {
            if s.is_empty() {
                "''".to_string()
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
    if table_name.starts_with(INTERNAL_PREFIX)
        || headers
            .iter()
            .any(|header| header.starts_with(INTERNAL_PREFIX))
    {
        return Err(
            "file names and CSV headers using Dataman's reserved undo prefix are not supported"
                .into(),
        );
    }
    let headers_string: String = headers
        .iter()
        .map(|header| format!("{} TEXT", quote_identifier(header)))
        .collect::<Vec<String>>()
        .join(",");
    log::info!("Creating table with headers: {}", headers_string);
    let query = format!(
        r#"CREATE TABLE IF NOT EXISTS {}
	(id INTEGER PRIMARY KEY, {})
	;"#,
        quote_identifier(table_name),
        headers_string
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
        .map(|header| quote_identifier(header))
        .collect::<Vec<String>>()
        .join(", ");
    Ok(columns)
}

pub(crate) fn sqlite_to_out(connection: &Connection, path: PathBuf) -> AppResult<()> {
    let mut stmt = connection
        .prepare(r#"SELECT "name" FROM sqlite_master WHERE type='table' AND name != 'table_of_tables' ORDER BY "name";"#)?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?
        .into_iter()
        .filter(|table| !table.starts_with(INTERNAL_PREFIX))
        .collect();
    let mut wtr = csv::Writer::from_path(path)?;
    for table in tables {
        let mut info = connection.prepare(&format!(
            "PRAGMA table_info(\"{}\")",
            table.replace('"', "\"\"")
        ))?;
        let columns = info
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|column| !column.starts_with(INTERNAL_PREFIX))
            .collect::<Vec<_>>();
        let select = columns
            .iter()
            .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");
        let query = &format!("SELECT {select} FROM \"{}\";", table.replace('"', "\"\""));
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
    insert_delimited_data_database(path, connection, b',')
}

pub(crate) fn insert_tsv_data_database(
    path: PathBuf,
    connection: &Connection,
) -> Result<(), AppError> {
    insert_delimited_data_database(path, connection, b'\t')
}

fn insert_delimited_data_database(
    path: PathBuf,
    connection: &Connection,
    delimiter: u8,
) -> Result<(), AppError> {
    let mut csv = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(&path)?;
    let table_name =
        Database::get_table_name(path).ok_or(app_error_other!("could not get table name."))?;
    let table_names = [table_name.to_string()];
    let mut queries = String::new();
    let query = create_table_query(&mut csv, &table_name)
        .map_err(|error| app_error_other!(error.to_string()))?;
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
        r#"INSERT INTO {} ({}) VALUES {};"#,
        quote_identifier(table_name),
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

pub(crate) fn save_to_csv_file(
    connection: &Connection,
    table_name: &str,
    save_file: &PathBuf,
) -> AppResult<()> {
    save_delimited_file(connection, table_name, save_file, b',')
}

pub(crate) fn save_to_tsv_file(
    connection: &Connection,
    table_name: &str,
    save_file: &PathBuf,
) -> AppResult<()> {
    save_delimited_file(connection, table_name, save_file, b'\t')
}

fn save_delimited_file(
    connection: &Connection,
    table_name: &str,
    save_file: &PathBuf,
    delimiter: u8,
) -> AppResult<()> {
    let mut info = connection.prepare(&format!(
        "PRAGMA table_info(\"{}\")",
        table_name.replace('"', "\"\"")
    ))?;
    let headers = info
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|header| header != "id" && !header.starts_with(INTERNAL_PREFIX))
        .collect::<Vec<_>>();
    let select = headers
        .iter()
        .map(|header| format!("\"{}\"", header.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let query = &format!(
        "SELECT {select} FROM \"{}\";",
        table_name.replace('"', "\"\"")
    );
    let mut stmt = connection.prepare(query).unwrap();

    // Collect all rows into a vector to avoid borrowing issues
    let rows: Vec<Vec<DataItem>> = stmt
        .query_map([], |row| {
            let mut items = Vec::new();
            for i in 0..headers.len() {
                let item = row.get_ref(i).unwrap();
                items.push(DataItem::from(item));
            }
            Ok(items)
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_path(save_file)?;
    wtr.write_record(&headers).unwrap();
    for row in rows {
        wtr.serialize(row).unwrap();
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::{assert_eq, println};

    #[test]
    fn empty_csv_fields_are_preserved_as_empty_text() {
        let mut reader = csv::ReaderBuilder::new().from_reader(b"name\n\"\"\n".as_slice());
        let record = reader.records().next().unwrap().unwrap();
        assert_eq!(value_query(&record), "('')");
    }

    #[test]
    fn csv_headers_with_quotes_are_imported_as_identifiers() {
        let table_name = format!("dataman-quoted-header-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("{table_name}.csv"));
        std::fs::write(&path, b"\"name \"\"quoted\"\"\"\nvalue\n").unwrap();
        let database = Database::try_from(vec![path.clone()]).unwrap();
        assert_eq!(
            database.get_headers(&table_name).unwrap(),
            vec!["id", "name \"quoted\""]
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tsv_import_export_round_trip() {
        let stem = format!("dataman-tsv-{}", std::process::id());
        let input = std::env::temp_dir().join(format!("{stem}.tsv"));
        let output = std::env::temp_dir().join(format!("{stem}-out.tsv"));
        std::fs::write(&input, b"name\tcount\nAda\t42\nGrace\t7\n").unwrap();

        let database = Database::try_from(vec![input.clone()]).unwrap();
        super::save_to_tsv_file(&database.connection, &stem, &output).unwrap();
        let reopened = Database::try_from(vec![output.clone()]).unwrap();
        assert_eq!(
            reopened
                .get_cell(&format!("{stem}-out"), 1, "name")
                .unwrap(),
            "Ada"
        );
        assert_eq!(
            reopened
                .get_cell(&format!("{stem}-out"), 2, "count")
                .unwrap(),
            "7"
        );

        std::fs::remove_file(input).unwrap();
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn write_db_to_out_test() {
        let database1 = Database::try_from(vec![PathBuf::from("assets/data.sqlite")]).unwrap();
        sqlite_to_out(&database1.connection, PathBuf::from("assets/out-data.csv")).unwrap();
        let database2 = Database::try_from(vec![PathBuf::from("assets/out-data.csv")]).unwrap();
        let first_row_db1 = database1.get(1, 0, "data", "").unwrap().1;
        let first_row_db2 = database2.get(1, 0, "out-data", "").unwrap().1;

        for (source_row, exported_row) in first_row_db1.iter().zip(&first_row_db2) {
            for (source_item, exported_item) in source_row.iter().zip(exported_row) {
                // CSV's empty field representation cannot distinguish SQL NULL
                // from empty text, so the importer reads it as empty text.
                if source_item == &DataItem::Null {
                    assert_eq!(exported_item, &DataItem::Text(String::new()));
                } else {
                    assert_eq!(source_item, exported_item);
                }
            }
        }
    }
}
