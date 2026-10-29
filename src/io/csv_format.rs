use std::path::PathBuf;
use std::{fs::File, path::Path};

use csv::{Reader, StringRecord};
use rusqlite::Connection;

use crate::error::{AppError, AppResult};
use crate::model::datarow::DataItem;

use crate::model::database::{Database, INTERNAL_PREFIX};

use crate::model::sql_queries::quote_identifier;

pub(crate) fn database_from_csv(path: PathBuf, connection: Connection) -> AppResult<Database> {
    insert_csv_data_database(path, &connection)?;
    Database::new(connection)
}

pub(crate) fn database_from_sqlite(connection: Connection) -> AppResult<Database> {
    let database = Database::new(connection)?;

    Ok(database)
}

pub(crate) fn create_table_query(csv: &mut Reader<File>, table_name: &str) -> AppResult<String> {
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

pub(crate) fn get_headers_for_query(csv: &mut Reader<File>, table_name: &str) -> AppResult<String> {
    let headers = csv.headers()?;
    let columns: String = headers
        .iter()
        .map(quote_identifier)
        .collect::<Vec<String>>()
        .join(", ");
    Ok(columns)
}

pub(crate) fn insert_csv_data_database(path: PathBuf, connection: &Connection) -> AppResult<()> {
    insert_delimited_data_database(path, connection, b',')
}

pub(crate) fn insert_tsv_data_database(path: PathBuf, connection: &Connection) -> AppResult<()> {
    insert_delimited_data_database(path, connection, b'\t')
}

fn insert_delimited_data_database(
    path: PathBuf,
    connection: &Connection,
    delimiter: u8,
) -> AppResult<()> {
    insert_delimited_cancellable(path, connection, delimiter, None)
}

pub(crate) fn insert_delimited_cancellable(
    path: PathBuf,
    connection: &Connection,
    delimiter: u8,
    cancellation: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> AppResult<()> {
    let context = format!("Importing '{}'", path.display());
    let owns_transaction = connection.is_autocommit();
    let mut transaction = None;
    let result = (|| -> AppResult<()> {
        if cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(AppError::Cancelled);
        }
        let mut csv = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .from_path(&path)?;
        let table_name = Database::get_table_name(path)
            .ok_or(AppError::invalid_input("could not get table name."))?;
        if owns_transaction {
            transaction = Some(connection.unchecked_transaction()?);
        }
        let query = create_table_query(&mut csv, &table_name)?;
        connection.execute_batch(&query)?;
        let columns = get_headers_for_query(&mut csv, &table_name)?;
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
                if cancellation
                    .is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed))
                {
                    return Err(AppError::Cancelled);
                }
                insert.execute(rusqlite::params_from_iter(record.iter()))?;
            }
        }
        if cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(AppError::Cancelled);
        }
        Ok(())
    })();
    let result = if let Some(mut transaction) = transaction {
        if cancellation.is_some() {
            connection.progress_handler(0, None::<fn() -> bool>);
        }
        let result = result.and_then(|()| {
            connection.execute_batch("COMMIT")?;
            Ok(())
        });
        let result = match result {
            Err(error) if !connection.is_autocommit() => crate::error::finish(
                Err(error),
                connection.execute_batch("ROLLBACK").map_err(AppError::from),
            ),
            result => result,
        };
        transaction.set_drop_behavior(rusqlite::DropBehavior::Ignore);
        if let Some(token) = cancellation {
            Database::install_cancellation(connection, token.clone());
        }
        result
    } else {
        result
    };
    result.map_err(|error| {
        error
            .normalize_cancelled(
                cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)),
            )
            .context(context)
    })
}

pub(crate) fn save_delimited_cancellable(
    connection: &Connection,
    table_name: &str,
    save_file: &PathBuf,
    delimiter: u8,
    cancellation: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> AppResult<()> {
    let check_cancelled = || -> AppResult<()> {
        if cancellation.is_some_and(|token| token.load(std::sync::atomic::Ordering::Relaxed)) {
            Err(AppError::Cancelled)
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
        crate::io::savers::tsv(&database, &stem, &output).unwrap();
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
        crate::io::savers::csv(&database1, "data", &output).unwrap();
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
    #[test]
    fn malformed_csv_retains_parser_error_and_file_context() {
        use std::error::Error;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.csv");
        std::fs::write(&path, "a,b\none,two\nbad\n").unwrap();
        let connection = Connection::open_in_memory().unwrap();
        let error = insert_csv_data_database(path.clone(), &connection).unwrap_err();
        assert!(error.to_string().contains(path.to_str().unwrap()));
        let source = error.source().unwrap();
        assert!(source
            .source()
            .unwrap()
            .downcast_ref::<csv::Error>()
            .is_some());
        assert!(connection.is_autocommit());
    }
}
