mod support;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use support::Fixture;

fn navigation(c: &mut Criterion) {
    let rows = support::rows();
    let mut group = c.benchmark_group(format!("rows/{rows}"));
    let fixture = Fixture::new(rows);
    let offset = (rows - 100) as u32;
    fixture
        .database
        .get(100, offset, "data", "ORDER BY value")
        .unwrap();
    group.bench_function("navigation/legacy_deep_sorted_offset", |b| {
        b.iter(|| {
            let mut statement = fixture
                .raw
                .prepare("SELECT id,value,amount FROM data ORDER BY value LIMIT 100 OFFSET ?")
                .unwrap();
            let mut rows = statement.query([offset]).unwrap();
            let mut page = Vec::new();
            while let Some(row) = rows.next().unwrap() {
                page.push((
                    row.get::<_, i64>(0).unwrap(),
                    row.get::<_, String>(1).unwrap(),
                    row.get::<_, i64>(2).unwrap(),
                ));
            }
            black_box(page);
        })
    });
    group.bench_function("navigation/cached_order_deep_page", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .database
                    .get(100, offset, "data", "ORDER BY value")
                    .unwrap(),
            );
        })
    });
    group.bench_function("navigation/rebuild_sort", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .database
                    .get(100, 0, "data", "ORDER BY amount DESC")
                    .unwrap(),
            );
            black_box(
                fixture
                    .database
                    .get(100, 0, "data", "ORDER BY value")
                    .unwrap(),
            );
        })
    });
    group.finish();
}
criterion_group! { name = benches; config = Criterion::default().sample_size(10); targets = navigation }
criterion_main!(benches);
