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
        .map(quote_identifier)
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
    insert_delimited_cancellable(path, connection, delimiter, None)
}

pub(crate) fn insert_delimited_cancellable(
    path: PathBuf,
    connection: &Connection,
    delimiter: u8,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), AppError> {
    let mut csv = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(&path)?;
    let table_name =
        Database::get_table_name(path).ok_or(app_error_other!("could not get table name."))?;
    let transaction = if connection.is_autocommit() {
        Some(connection.unchecked_transaction()?)
    } else {
        None
    };
    let query = create_table_query(&mut csv, &table_name)
        .map_err(|error| app_error_other!(error.to_string()))?;
    connection.execute_batch(&query)?;
    let columns = get_headers_for_query(&mut csv, &table_name)
        .map_err(|error| app_error_other!(error.to_string()))?;
    let placeholders = (0..csv.headers()?.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    {
        let mut insert = connection.prepare(&format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders})",
            quote_identifier(&table_name)
        ))?;
        let mut record = StringRecord::new();
        while csv.read_record(&mut record)? {
            if cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(AppError::from("Operation cancelled"));
            }
            insert.execute(rusqlite::params_from_iter(record.iter()))?;
        }
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
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
    save_delimited_cancellable(connection, table_name, save_file, delimiter, None)
}

pub(crate) fn save_delimited_cancellable(
    connection: &Connection,
    table_name: &str,
    save_file: &PathBuf,
    delimiter: u8,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> AppResult<()> {
    let check_cancelled = || -> AppResult<()> {
        if cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)) {
            Err(AppError::from("Operation cancelled"))
        } else {
            Ok(())
        }
    };
    check_cancelled()?;
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
    let mut stmt = connection.prepare(query)?;

    let parent = save_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut wtr = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(temporary.as_file_mut());
        wtr.write_record(&headers)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            check_cancelled()?;
            let items = (0..headers.len())
                .map(|i| row.get_ref(i).map(DataItem::from))
                .collect::<Result<Vec<_>, _>>()?;
            wtr.serialize(items)?;
        }
        wtr.flush()?;
    }
    temporary.as_file().sync_all()?;
    check_cancelled()?;
    temporary
        .persist(save_file)
        .map_err(|e| AppError::Io(e.error))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::{assert_eq, println};

    #[test]
    fn empty_csv_fields_are_preserved_as_empty_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.csv");
        std::fs::write(&path, b"name\n\"\"\n").unwrap();
        let database = Database::try_from(vec![path]).unwrap();
        assert_eq!(
            database.get_cell_item("empty", 1, "name").unwrap(),
            DataItem::Text(String::new())
        );
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
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out-data.csv");
        let database1 = Database::try_from(vec![PathBuf::from("assets/data.csv")]).unwrap();
        sqlite_to_out(&database1.connection, output.clone()).unwrap();
        let database2 = Database::try_from(vec![output]).unwrap();
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
