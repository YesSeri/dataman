use std::path::PathBuf;
use std::{error::Error, fs::File, path, path::Path};

use csv::{Reader, StringRecord, Writer};
use rusqlite::{Connection, Rows};
use serde::Serialize;

use crate::error::AppResult;
use crate::model::datarow::DataItem;

use super::database::Database;

pub(crate) fn database_from_csv(
    path: PathBuf,
    connection: Connection,
) -> crate::error::AppResult<Database> {
    //let mut csv = csv::Reader::from_path(path)?;
    let mut csv = csv::ReaderBuilder::new().from_path(&path)?;
    let table_name = Database::get_table_name(path);
    let table_names = vec![table_name.to_string()];
    let mut queries = String::new();
    let query = build_create_table_query(&mut csv, &table_name).unwrap();
    queries.push_str(&query);
    connection.execute_batch(&queries)?;
    queries.clear();
    let columns = get_headers_for_query(&mut csv, &table_name).unwrap();
    let mut database = Database::new(connection)?;
    let limit = 10000;
    let mut i = 0;

    let records = csv.records();
    // let capacity = if len < limit { len + 3 } else { limit + 3 };
    let mut items = Vec::with_capacity(limit);
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

pub(crate) fn sqlite_to_out(connection: &Connection, path: path::PathBuf) -> AppResult<()> {
    let mut stmt =
        connection.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name;")?;

    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut wtr = csv::Writer::from_path(path)?;
    for table in tables {
        let query = &format!("SELECT * FROM `{}`;", table);
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
                dbg!(&header);
                dbg!(&item);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::{assert_eq, println};

    #[test]
    fn write_db_to_out_test() {
        let mut database1 = Database::try_from(Path::new("assets/data.sqlite")).unwrap();
        sqlite_to_out(&database1.connection, PathBuf::from("assets/out-data.csv")).unwrap();
        let mut database2 = Database::try_from(Path::new("assets/out-data.csv")).unwrap();
        let first_row_db1 = database1.get(1, 0, "data".to_string()).unwrap().1;
        let first_row_db2 = database2.get(1, 0, "out-data".to_string()).unwrap().1;

        dbg!(&first_row_db1);
        dbg!(&first_row_db2);

        for (i, item) in first_row_db1.iter().enumerate() {
            assert_eq!(item, first_row_db2.get(i).unwrap());
        }
    }
}
