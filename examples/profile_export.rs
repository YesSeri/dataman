//! Run with /usr/bin/time -v to compare streaming export with the former collect-all path.
use dataman::model::{database::Database, datarow::DataItem};
use rusqlite::Connection;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let rows: usize = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("100000")
        .parse()?;
    let columns: usize = args.get(2).map(String::as_str).unwrap_or("16").parse()?;
    let legacy = args.get(3).is_some_and(|arg| arg == "legacy");
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source.db");
    let output = directory.path().join("output.csv");
    let connection = Connection::open(&source)?;
    let declarations = (0..columns)
        .map(|i| format!("c{i} TEXT"))
        .collect::<Vec<_>>()
        .join(",");
    let values = (0..columns)
        .map(|_| "'abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ__'")
        .collect::<Vec<_>>()
        .join(",");
    connection.execute_batch(&format!(
        "CREATE TABLE data(id INTEGER PRIMARY KEY,{declarations}); BEGIN;
        WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<{rows})
        INSERT INTO data SELECT id,{values} FROM n; COMMIT;"
    ))?;
    let started = Instant::now();
    if legacy {
        let columns_sql = (0..columns)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = connection.prepare(&format!("SELECT {columns_sql} FROM data"))?;
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|i| row.get_ref(i).map(DataItem::from))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut writer = csv::Writer::from_path(&output)?;
        writer.write_record((0..columns).map(|i| format!("c{i}")))?;
        for row in rows {
            writer.serialize(row)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
    } else {
        Database::new(connection)?.export_delimited("data", &output, b',')?;
    }
    eprintln!(
        "mode={} rows={rows} columns={columns} export_ms={:.1}",
        if legacy { "legacy" } else { "streaming" },
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
