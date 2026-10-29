mod support;
use criterion::{criterion_group, criterion_main, Criterion};
use support::Fixture;

fn regex(c: &mut Criterion) {
    let rows = support::rows();
    let mut group = c.benchmark_group(format!("rows/{rows}"));
    let fixture = Fixture::new(rows);
    group.bench_function("regex/filter_cached_pattern", |b| b.iter(|| {
        fixture.database.execute_batch("CREATE TABLE filtered AS SELECT * FROM data WHERE value REGEXP '99$'; DROP TABLE filtered;").unwrap();
    }));
    group.bench_function("regex/derive_capture", |b| b.iter(|| {
        fixture.database.execute_batch("ALTER TABLE data ADD COLUMN transformed TEXT;
            UPDATE data SET transformed=regexp_transform_with_capture_group('(value)-(.*)',value,'$2:$1');
            ALTER TABLE data DROP COLUMN transformed;").unwrap();
    }));
    group.finish();
}
criterion_group! { name = benches; config = Criterion::default().sample_size(10); targets = regex }
criterion_main!(benches);
