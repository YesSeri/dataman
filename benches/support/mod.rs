#![allow(dead_code)]
use dataman::model::database::Database;
use rusqlite::Connection;

pub fn rows() -> usize {
    std::env::var("DATAMAN_BENCH_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000)
}
pub struct Fixture {
    pub database: Database,
    pub raw: Connection,
    pub directory: tempfile::TempDir,
}
impl Fixture {
    pub fn new(rows: usize) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.db");
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(&format!(
            "CREATE TABLE data(id INTEGER PRIMARY KEY, value TEXT, amount INTEGER);
            BEGIN; WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<{rows})
            INSERT INTO data SELECT id, 'value-' || (id % 1000), id * 2 FROM n; COMMIT;"
        ))
        .unwrap();
        let database = Database::new(Connection::open(&path).unwrap()).unwrap();
        Self {
            database,
            raw,
            directory,
        }
    }
}
