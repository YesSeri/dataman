use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::Connection;
use rusqlite::Result;

fn setup_large_database(conn: &Connection, num_rows: usize) -> Result<()> {
    conn.execute(
        "CREATE TABLE test (id INTEGER PRIMARY KEY, val INTEGER, transformed_val INTEGER)",
        [],
    )?;
    let mut stmt = conn.prepare("INSERT INTO test (val) VALUES (?)")?;

    for i in 0..num_rows {
        let val = i + 1;
        stmt.execute([val])?;
    }

    Ok(())
}
fn benchmark_math(c: &mut Criterion) {
    let conn0 = Connection::open_in_memory().unwrap();
    setup_large_database(&conn0, 100000).unwrap();
    let conn1 = Connection::open_in_memory().unwrap();
    setup_large_database(&conn1, 100000).unwrap();
    c.bench_function("math complex query", |b| {
        b.iter(|| {
            let mut stmt = conn1
                .prepare("UPDATE test SET transformed_val = ln(val)")
                .unwrap();
            stmt.execute([]).unwrap();
        })
    });
    c.bench_function("math simple query", |b| {
        b.iter(|| {
            let mut stmt = conn0
                .prepare("UPDATE test SET transformed_val = val + 1")
                .unwrap();
            stmt.execute([]).unwrap();
        })
    });
}

criterion_group!(benches, benchmark_math);
criterion_main!(benches);
