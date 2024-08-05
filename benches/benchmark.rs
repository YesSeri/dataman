use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::Connection;
use rusqlite::Result;
use std::time::Instant;

fn setup_large_database(conn: &Connection, num_rows: usize) -> Result<()> {
    conn.execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, val INT, transformed_val INT)",
        [],
    )?;
    let mut stmt = conn.prepare("INSERT INTO test (val) VALUES (?)")?;

    for i in 0..num_rows {
        let val = i;
        stmt.execute([val])?;
    }

    Ok(())
}
fn benchmark_math(c: &mut Criterion) {
    let conn0 = Connection::open_in_memory().unwrap();
    setup_large_database(&conn0, 100000).unwrap();
    let conn1 = Connection::open_in_memory().unwrap();
    setup_large_database(&conn1, 100000).unwrap();
    c.bench_function("math sql query", |b| {
        b.iter(|| {
            let start = Instant::now();
            let mut stmt = conn0
                .prepare("UPDATE test SET transformed_val = ?")
                .unwrap();
            let math_expr = "val + 1";
            stmt.execute([math_expr]).unwrap();
            start.elapsed()
        })
    });

    c.bench_function("math compiled function", |b| {
        b.iter(|| {
            let start = Instant::now();
            let mut stmt = conn1
                .prepare("UPDATE test SET transformed_val = ?")
                .unwrap();
            let math_expr = "val / 7 + 2 ";
            stmt.execute([math_expr]).unwrap();
            start.elapsed()
        })
    });
}

criterion_group!(benches, benchmark_math);
criterion_main!(benches);
