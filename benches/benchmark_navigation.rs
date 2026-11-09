mod support;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dataman::{controller::controller_impl::Controller, model::database::Database};
use support::Fixture;

fn navigation(c: &mut Criterion) {
    let rows = support::rows();
    let mut group = c.benchmark_group(format!("rows/{rows}"));
    let mut fixture = Fixture::new(rows);
    let source = fixture.directory.path().join("fixture.db");
    group.bench_function("startup/open_to_first_usable_page", |b| {
        b.iter(|| {
            // Include read-only opening, schema discovery, controller
            // construction and first page; promotion and count are measured separately.
            let database = Database::try_from(vec![source.clone()]).unwrap();
            let mut controller = Controller::new(database).unwrap();
            black_box(controller.get_headers_and_rows(100).unwrap());
        })
    });
    group.bench_function("startup/first_edit_promotion", |b| {
        b.iter_batched(
            || {
                let mut db = Database::try_from(vec![source.clone()]).unwrap();
                let sheet = db.open_sheet("data").unwrap();
                let page = db.fetch_page(sheet, 100, 0, "").unwrap();
                let column = db.column_id(sheet, "value").unwrap();
                (db, sheet, page.row_ids[0], column)
            },
            |(mut db, sheet, row, column)| {
                db.edit_row(sheet, row, column, "edited").unwrap();
                black_box(db);
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.bench_function("startup/explicit_count", |b| {
        b.iter_batched(
            || Database::try_from(vec![source.clone()]).unwrap(),
            |db| black_box(db.count_rows("data").unwrap()),
            criterion::BatchSize::SmallInput,
        );
    });
    let offset = (rows - 100) as u32;
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
        fixture
            .database
            .get(100, offset, "data", "ORDER BY value")
            .unwrap();
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
    group.bench_function("navigation/identity_keyset_page", |b| {
        let sheet = fixture.database.open_sheet("data").unwrap();
        b.iter_batched(
            || {
                fixture.database.fetch_page(sheet, 100, 0, "").unwrap();
            },
            |_| {
                black_box(fixture.database.fetch_page(sheet, 100, 100, "").unwrap());
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.bench_function("navigation/identity_cached_sorted_page", |b| {
        let sheet = fixture.database.open_sheet("data").unwrap();
        fixture
            .database
            .fetch_page(sheet, 100, offset, "ORDER BY value")
            .unwrap();
        b.iter(|| {
            black_box(
                fixture
                    .database
                    .fetch_page(sheet, 100, offset, "ORDER BY value")
                    .unwrap(),
            );
        });
    });
    group.finish();
}
criterion_group! { name = benches; config = Criterion::default().sample_size(10); targets = navigation }
criterion_main!(benches);
