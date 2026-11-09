//! Run in an isolated TEMP/TMPDIR directory; fixture generation belongs outside
//! this process. scripts/measure_open.py records process RSS separately.
use dataman::model::database::{Database, StorageState};
use std::{path::Path, time::Instant};

fn disk_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                disk_bytes(&path)
            } else {
                path.metadata().unwrap().len()
            }
        })
        .sum()
}

fn main() {
    let path = std::env::args_os().nth(1).expect("source.sqlite");
    let started = Instant::now();
    let mut db = Database::try_from(vec![path.into()]).unwrap();
    let sheet = db.open_sheet("data").unwrap();
    let page = db.fetch_page(sheet, 100, 0, "").unwrap();
    let first_page_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(db.storage_state(), StorageState::ReadOnlySource);
    let browse_temp_bytes = disk_bytes(&std::env::temp_dir());
    let started = Instant::now();
    for _ in 0..100 {
        std::hint::black_box(db.fetch_page(sheet, 100, 100, "").unwrap());
    }
    let navigation_ms = started.elapsed().as_secs_f64() * 10.0;
    let started = Instant::now();
    let count = db.count_rows("data").unwrap();
    let count_ms = started.elapsed().as_secs_f64() * 1000.0;
    let column = db.column_id(sheet, "value").unwrap();
    let started = Instant::now();
    db.edit_row(sheet, page.row_ids[0], column, "edited")
        .unwrap();
    let promotion_ms = started.elapsed().as_secs_f64() * 1000.0;
    let workspace_temp_bytes = disk_bytes(&std::env::temp_dir());
    println!("{{\"rows\":{count},\"first_page_ms\":{first_page_ms:.3},\"first_edit_ms\":{promotion_ms:.3},\"count_ms\":{count_ms:.3},\"page_ms\":{navigation_ms:.3},\"browse_temp_bytes\":{browse_temp_bytes},\"workspace_temp_bytes\":{workspace_temp_bytes}}}");
}
