mod support;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dataman::{
    controller::{controller_impl::Controller, Command},
    model::database::Database,
    navigation::Direction,
};
use support::Fixture;

fn workflows(c: &mut Criterion) {
    let rows = support::rows();
    let mut group = c.benchmark_group(format!("rows/{rows}"));
    let fixture = Fixture::new(rows);
    let output = fixture.directory.path().join("export.csv");
    fixture
        .database
        .export_delimited("data", &output, b',')
        .unwrap();
    group.bench_function("workflow/import_csv", |b| {
        b.iter(|| {
            black_box(Database::try_from(vec![output.clone()]).unwrap());
        })
    });
    group.bench_function("workflow/export_csv", |b| {
        b.iter(|| {
            fixture
                .database
                .export_delimited("data", &output, b',')
                .unwrap()
        })
    });
    group.bench_function("workflow/derive_column", |b| {
        b.iter(|| {
            fixture
                .database
                .derive_column("data", "value", "derived", |value| {
                    Some(format!("{value}-suffix"))
                })
                .unwrap();
            fixture
                .database
                .execute_batch("ALTER TABLE data DROP COLUMN derived;")
                .unwrap();
        })
    });
    // Model the original transaction-per-row update on the same disk-backed fixture.
    let mut legacy_iteration = false;
    group.bench_function("bulk/legacy_1000_commits", |b| {
        b.iter(|| {
            legacy_iteration = !legacy_iteration;
            for id in 1..=1000 {
                fixture
                    .raw
                    .execute(
                        "UPDATE data SET value=? WHERE id=?",
                        rusqlite::params![
                            if legacy_iteration {
                                "legacy-a"
                            } else {
                                "legacy-b"
                            },
                            id
                        ],
                    )
                    .unwrap();
            }
        })
    });
    group.bench_function("workflow/exact_search", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .database
                    .exact_search("data", "ORDER BY id", 0, "value", "value-999")
                    .unwrap(),
            );
        })
    });
    let mut controller = Controller::new(fixture.database);
    controller
        .execute(Command::Move(Direction::Right), vec![])
        .unwrap();
    controller
        .execute(Command::SelectSql, vec!["id <= 1000".into()])
        .unwrap();
    group.bench_function("bulk/transaction_with_undo_1000", |b| {
        b.iter(|| {
            controller
                .execute(Command::BulkEdit, vec!["updated".into()])
                .unwrap();
            controller.execute(Command::Undo, vec![]).unwrap();
        })
    });
    group.bench_function("workflow/regex_selection", |b| {
        b.iter(|| {
            controller
                .execute(Command::SelectRegex, vec!["value-99$".into()])
                .unwrap();
            controller.execute(Command::ClearSelection, vec![]).unwrap();
        })
    });
    group.finish();
}
criterion_group! { name = benches; config = Criterion::default().sample_size(10); targets = workflows }
criterion_main!(benches);
