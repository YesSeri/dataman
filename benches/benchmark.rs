use criterion::{criterion_group, criterion_main, Criterion};
use dataman::model::regexping::custom_functions::add_custom_functions;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result};
use std::{
    thread::{self},
    time::Instant,
};

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
        .max_size(4)
        .connection_customizer(Box::new(CustomFunctionAdder))
        .build(manager)
        .unwrap();

    let num_rows = 1000000;
    setup_large_database(&pool, num_rows).unwrap();

    c.bench_function("regexp transform single-threaded", |b| {
        b.iter(|| {
            let conn = pool.get().unwrap();
            let regex_str = r"\d+";
            let mut stmt = conn
                .prepare(
                    "UPDATE test SET transformed_text = regexp_transform_no_capture_group(?, text)",
                )
                .unwrap();
            stmt.execute([regex_str]).unwrap();
        })
    });

    c.bench_function("regexp transform multi-threaded", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            let handles: Vec<_> = (0..4)
                .map(|i| {
                    let pool = pool.clone();
                    let start = i * (num_rows / 4) + 1; // +1 to start from 1 since id starts from 1
                    let end = if i == 3 { num_rows } else { start + (num_rows / 4) - 1 };
                    thread::spawn(move || {
                        for _ in 0..iters {
                            let conn = pool.get().unwrap();
                            let regex_str = r"\d+";
                            let mut stmt = conn.prepare("UPDATE test SET transformed_text = regexp_transform_no_capture_group(?, text) WHERE id BETWEEN ? AND ?").unwrap();
                            stmt.execute((regex_str, start, end)).unwrap();
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
            start.elapsed()
        })
    });
}

criterion_group!(benches, benchmark_regexp_transform_no_capture_group);
criterion_main!(benches);
