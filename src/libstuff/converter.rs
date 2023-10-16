use std::path::Path;

use super::{datarow::DataRow, db::Database};

pub(crate) fn database_from_csv(path: &Path) -> crate::error::AppResult<Database> {
    let mut csv = csv::Reader::from_path(path)?;
    let table_name = Database::get_table_name(path).unwrap();
    let table_names = vec![table_name.to_string()];
    let mut database = Database::new(table_names)?;
    let funs = vec![
        Database::build_create_table_query,
        Database::build_add_data_query,
    ];
    let mut queries = String::new();
    // queries.
    for fun in funs {
        let query = fun(&mut csv, table_name).unwrap();
        queries.push_str(&query);
        // database.execute(&query, ())?;
    }
    database.execute_batch(&queries)?;
    let query =
        "SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;".to_string();
    let table_idx: usize = database
        .connection
        .query_row(&query, [], |row| row.get(0))?;
    database.current_table_idx = table_idx;

    let query = format!(
        "SELECT * FROM `{}` ORDER BY rowid ASC LIMIT 50 OFFSET 0;",
        table_name,
    );
    let mut result = Vec::new();
    let mut headers = Vec::new();
    {
        let mut stmt = database.connection.prepare(&query)?;
        for header in stmt.column_names().iter() {
            headers.push(header.to_string());
        }

        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let data: DataRow = DataRow::from(row);
            result.push(data);
        }
    }
    Ok(database)
}

pub(crate) fn database_from_sqlite(path: &Path) -> crate::error::AppResult<Database> {
    let mut csv = csv::Reader::from_path(path)?;
    let table_name = Database::get_table_name(path).unwrap();
    let table_names = vec![table_name.to_string()];
    let mut database = Database::new(table_names)?;
    let funs = vec![
        Database::build_create_table_query,
        Database::build_add_data_query,
    ];
    let mut queries = String::new();
    // queries.
    for fun in funs {
        let query = fun(&mut csv, table_name).unwrap();
        queries.push_str(&query);
        // database.execute(&query, ())?;
    }

    database.execute_batch(&queries)?;
    let query =
        "SELECT rowid FROM sqlite_master WHERE type='table' ORDER BY rowid LIMIT 1;".to_string();
    let table_idx: usize = database
        .connection
        .query_row(&query, [], |row| row.get(0))?;
    database.current_table_idx = table_idx;
    Ok(database)
}
