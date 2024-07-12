use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rusqlite::{Connection, Result};

pub struct Database {
    pub connection: Connection,
}

impl Database {
    pub fn new(connection: Connection) -> Result<Self> {
        Ok(Database { connection })
    }
}

fn setup_large_database(num_rows: usize) -> Result<Database> {
    let conn = Connection::open_in_memory()?;
    {
        conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY, text TEXT)", [])?;
        let mut stmt = conn.prepare("INSERT INTO test (text) VALUES (?)")?;

        for i in 0..num_rows {
            let text = format!("Sample text with number {}", i);
            stmt.execute([text])?;
        }
    }
    let database = Database::new(conn).unwrap();

    Ok(database)
}

use std::time::Instant;
fn large_set_test(num_rows: usize) -> Result<(), rusqlite::Error> {
    let database = setup_large_database(num_rows)?;
    let conn = &database.connection;

    let mut stmt = conn.prepare("SELECT regexp(?, text) FROM test")?;
    let regex_str = r"\d+";

    let start = Instant::now();
    let mut count = 0;
    let mut rows = stmt.query([regex_str])?;
    while let Some(row) = rows.next()? {
        let result: bool = row.get(0)?;
        if result {
            count += 1;
        }
    }
    let duration = start.elapsed();

    println!("Large dataset test duration: {:?}", duration);
    println!("Number of matches: {}", count);

    Ok(())
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("large set 10000", |b| {
        b.iter(|| large_set_test(black_box(10000)))
    });

    c.bench_function("large set 100000", |b| {
        b.iter(|| large_set_test(black_box(100000)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
