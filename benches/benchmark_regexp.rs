use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use dataman::model::regexping::custom_functions::add_custom_functions;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};

fn setup_large_database(pool: &Pool<SqliteConnectionManager>, num_rows: usize) -> Result<()> {
    let conn = pool.get().unwrap();
    conn.execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, text TEXT, transformed_text TEXT)",
        [],
    )?;
    let mut stmt = conn.prepare("INSERT INTO test (text) VALUES (?)")?;

    for i in 0..num_rows {
        let text = format!("Sample text with number {}", i);
        stmt.execute([text])?;
    }

    Ok(())
}

use r2d2::CustomizeConnection;
#[derive(Debug)]
struct CustomFunctionAdder;

impl CustomizeConnection<Connection, rusqlite::Error> for CustomFunctionAdder {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), rusqlite::Error> {
        add_custom_functions(conn)
    }
}

fn benchmark_regexp_transform_no_capture_group(c: &mut Criterion) {
    let manager = SqliteConnectionManager::memory();

    let pool = Pool::builder()
        .max_size(8)
        .connection_customizer(Box::new(CustomFunctionAdder))
        .build(manager)
        .unwrap();

    let num_rows = 100000;
    setup_large_database(&pool, num_rows).unwrap();

    let mut group = c.benchmark_group("Regexp Transform");

    group.bench_with_input(
        BenchmarkId::new("Transform V1", num_rows),
        &num_rows,
        |b, &_num_rows| {
            b.iter(|| {
                let conn = pool.get().unwrap();
                let regex_str = r"\d+";
                let mut stmt = conn
                    .prepare("UPDATE test SET transformed_text = regexp(?, text)")
                    .unwrap();
                stmt.execute([regex_str]).unwrap();
            })
        },
    );

    group.bench_with_input(
        BenchmarkId::new("Transform V2", num_rows),
        &num_rows,
        |b, &_num_rows| {
            b.iter(|| {
                let conn = pool.get().unwrap();
                let regex_str = r"\d+";
                let mut stmt = conn
                    .prepare("UPDATE test SET transformed_text = regexp_v2(?, text)")
                    .unwrap();
                stmt.execute([regex_str]).unwrap();
            })
        },
    );

    group.finish();
}

criterion_group!(benches, benchmark_regexp_transform_no_capture_group);
criterion_main!(benches);
