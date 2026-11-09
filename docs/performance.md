# Non-blocking MVP performance diagnostics

Functional P0 sign-off requires the Windows/Linux/macOS matrix. Performance
measurements do not block the MVP. Slow runs and outliers remain in raw JSONL and
become follow-up work; none establish an advantage over VisiData.

Build before measuring, and run one measurement process at a time:

```powershell
cargo build --locked --release --bin dataman --example profile_open
python scripts/measure_open.py target/release/examples/profile_open.exe --jsonl docs/p0-diagnostics.jsonl
python scripts/measure_open.py target/release/examples/profile_open.exe --terminal-binary target/release/dataman.exe --rows 10000000 --shape narrow --jsonl docs/p0-diagnostics-terminal.jsonl
python scripts/terminal_acceptance.py target/release/dataman.exe 100000 --shape wide --repetitions 5 --jsonl docs/p0-terminal-wide.jsonl
```

Omit `.exe` on Unix. Both tools preserve their original positional arguments and
support `--shape`, `--repetitions`, `--working-directory`, and `--jsonl`.
`measure_open.py` defaults to five measured runs for every 100K/1M/10M case across
narrow (the original two-column terminal fixture), wide (16 mixed columns), and
skewed (90% repeated, 9% unique, 1% null value cells) shapes. Fixtures are generated
before measurement, sequentially, with conservative source/candidate/journal/disk
headroom preflight. Insufficient space is a skipped diagnostic. Local functional acceptance
always treats an inability to complete functional checks as a failure.

Results report samples, median, minimum and maximum; nearest-rank p95 is reported
only with at least 20 samples. Failed runs and outliers remain present. Warm
opening <=1s, verified cold opening <=2s, cached navigation <=50ms and cancellation
acknowledgement <=250ms are diagnostic p95 targets, **unassessed** below 20 samples.
Promotion/count throughput and resource use have no release-blocking limits.

Library measurements cover opening and a 100-row page, repeated page fetch, first
edit including promotion, and explicit count. Terminal measurements cover actual
launch-to-painted-page, cached cursor movement, first edit, F6 and SQL cancellation.
When `--terminal-binary` is supplied, the measurement driver additionally attempts
active F6 and copying cancellation per case. The terminal observer cannot guarantee
that a fast operation is still scanning/copying when Esc is handled; deterministic
barrier regressions establish correctness. Missed active phases are retained as
failed diagnostic observations rather than silently measuring cancellation before
work begins. A subsequent count/promotion retry is required after cancellation.

The actual child process's peak RSS comes from Windows per-process peak working
set counters or Unix per-child `wait4` usage (macOS bytes, Linux KiB converted to
bytes). Isolated session-directory file sizes are sampled every 10 ms and reported
as **sampled temporary-disk high-water mark**: brief peaks may be missed. Metadata
records OS, CPU, RAM, filesystem, fixture bytes, toolchain, SHA, sampling interval,
and cache conditions. A session directory excludes fixture storage; fixture size
is recorded separately. No caches are automatically flushed. Ordinary runs are
warm/cache-uncontrolled. Fresh processes and copied fixtures are not cold-cache
measurements; verified cold runs require a recorded operator-controlled procedure.

Current measurements, limitations and follow-up work are recorded in
[P0 evidence](p0-evidence.md). Earlier single-run observations below are historical
and do not establish p95 or cross-platform acceptance.

# Database execution and performance

The terminal thread owns input, editing buffers, cached navigation, and rendering.
A single database worker owns the synchronous controller, SQLite connection,
undo records, temporary session storage, and file operations. No database handle
is present in a render snapshot.

Requests capture their selected IDs and cursor position. Both requests and
responses use bounded channels; only one data operation may be outstanding.
Operation IDs and view generations prevent stale completions from replacing a
newer view. Busy navigation retains only its latest pending request. Progress
phase messages are best effort; completion messages are always delivered.

Esc cancels the active operation through an operation-specific atomic token.
Explicit counts use an aggregate scan with progress callbacks rather than the
single optimized btree-count opcode, so cancellation can interrupt a long count.
Source busy waits are bounded at 50 ms because busy handlers do not invoke
SQLite progress callbacks.
SQLite's progress handler checks it during SQL, and Rust import, export, regex,
and derivation loops check it between records or batches. Transactions roll back
on failure or cancellation. Progress interruption is disabled during the final
commit/rollback boundary. A cancellation after that boundary does not undo an
already completed edit. Errors refreshing its page are reported separately.

Imported datasets live in unique temporary directories, with the connection
closed before cleanup. Compatible SQLite inputs browse directly from read-only
sources. The first writable operation copies a consistent snapshot to an
unpublished private workspace; later writes reuse it. Source reads use scoped
transactions, with generation checks before and after each operation. Views and
sources requiring snapshots can promote during sheet opening. CSV/TSV export and SQLite backup use a temporary sibling output and
replace the destination after successful completion. The active SQLite source
cannot be replaced by an export.

## Caching and memory

Natural rowid and primary-key tuple orders fetch directly through SQLite indexes.
Adjacent pages use scalar or tuple keyset cursors; arbitrary jumps use indexed
`LIMIT/OFFSET`. These orders do not allocate a positional map. Other sorts retain
cancellable position maps containing native locators in the session database.
Opaque handles are interned only for fetched or selected rows. Row maps and
ordering maps use a separate, named temporary SQLite file with a 2 MiB page cache;
this storage is separate from source data and is removed on exit.
The sidecar retains its disk rollback journal but disables durable fsync: its
records are session-only and are discarded after a crash. Cancellation and
failed operations still roll back their data and identity changes together.
This setting does not change the separate output-file sync used by exports.

The `startup/open_to_first_usable_page` navigation benchmark includes loading,
schema discovery, controller construction, first-page fetch, and session cleanup.
It excludes workspace promotion and explicit counting, which have separate
`startup/first_edit_promotion` and `startup/explicit_count` benchmarks. Fixture generation is outside
the timed region. It does not include terminal painting or process startup.

- Schemas, table names, and row counts are cached. Cell value changes retain
  schema/count caches and preserve an existing ID ordering map when triggers and
  foreign-key cascades cannot affect other rows or tables.
- Each active sorted sheet can retain an indexed position map of native storage
  locators. Value changes invalidate these maps; sorting never defines row
  identity. Cached rendering and selection use handles carried with the page.
- The active source page contains at most 100 rows. Worker and render snapshots
  share its immutable cell data through `Arc`; visible columns use index mappings.
  Layout changes reuse that source page, and widths are calculated per snapshot.
- Selection snapshots use copy-on-write sharing. Selection membership necessarily
  uses memory proportional to the number of selected IDs, but selection operations
  do not load complete source rows into Rust. There is no million-row cutoff.
- Large bulk-edit and deleted-row undo payloads live in reserved SQLite tables.
  Expiration and redo invalidation remove these tables. Exported databases omit
  reserved undo artifacts.
- CSV/TSV inserts bind values to one reusable statement in a transaction.
  Derivation uses bounded batches; exports serialize one row at a time. Regex SQL
  functions cache compiled patterns using SQLite argument auxiliary data.

The initial sort-map build and a rebuild after a sort-affecting change remain
proportional to the table size. They run on the worker and can be cancelled.
Editable sheets use hidden SQLite rowids or non-null primary-key tuples, with
private snapshots for sources without a usable key. Signed SQLite rowids,
including both `i64` extremes, are supported. Runtime row handles are independent
of those values. Row counts/page offsets retain the existing `u32` interface.

## Validation and reproduction

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release

# Exercise each workflow once at both dataset sizes.
cargo bench --bench benchmark --bench benchmark_navigation --bench benchmark_regexp -- --test
DATAMAN_BENCH_ROWS=1000000 cargo bench --bench benchmark --bench benchmark_navigation --bench benchmark_regexp -- --test

# Measure deep navigation; benchmark groups include dataset size.
cargo bench --bench benchmark_navigation -- deep --warm-up-time 0.1 --measurement-time 0.5 --noplot
DATAMAN_BENCH_ROWS=1000000 cargo bench --bench benchmark_navigation -- deep --warm-up-time 0.1 --measurement-time 0.5 --noplot

# Measure cached rendering with a terminal test backend.
cargo test --release benchmark_cached_render -- --ignored --nocapture

# Compare export memory with the former collect-all behavior.
cargo build --release --example profile_export
/usr/bin/time -f 'peak_rss_kib=%M elapsed_s=%e' target/release/examples/profile_export 100000 16 legacy
/usr/bin/time -f 'peak_rss_kib=%M elapsed_s=%e' target/release/examples/profile_export 100000 16 streaming
```

Benchmarks use temporary disk-backed fixtures. The workflow suite covers import,
export, derivation, bulk edits with undo, exact search, and regex selection. The
regex suite exercises production scalar functions. Navigation compares the
former deep sorted `LIMIT/OFFSET` query against cached position lookup; its
cached lookup timing excludes the initial map construction. The legacy bulk
comparison performs 1,000 individual commits; the new comparison includes both
the edit and undo.

## Copy-on-first-write observations (2026-11-09)

These are single-run local Windows observations from the optimized build on the
100K/1M/10M three-column fixture, with filesystem caches left intact. No build ran
during the recorded matrix. Fixture generation is excluded. The raw results are
[p0-measurements-2026-11-09.jsonl](p0-measurements-2026-11-09.jsonl).

| Rows | Library first page | First edit including promotion | Explicit count | Repeated page fetch | Process peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 100K | 34.4 ms | 99.4 ms | 3.34 ms | 0.926 ms | 9.46 MiB |
| 1M | 21.4 ms | 599.3 ms | 24.17 ms | 0.913 ms | 11.24 MiB |
| 10M | 36.1 ms | 6,636.7 ms | 258.00 ms | 0.950 ms | 11.27 MiB |

Library first page measures source opening, sheet discovery, and 100-row fetch;
it does not include process launch, terminal painting, or controller construction.
The repeated-page figure averages 100 fetches of a previously visited natural
page; it still queries SQLite. Peak RSS is the profile process high-water mark
across browsing, counting, and promotion, sampled through Windows process
counters at 1 ms intervals. It excludes the Python fixture/harness process and
does not establish the terminal process's peak memory.

Retained browsing storage after the first page was **20 KiB at every size**.
Retained workspace storage after the first edit was 2.11, 21.05, and 223.52 MiB,
respectively. Source fixtures were 2.09, 21.03, and 223.50 MiB. These disk readings
are phase-boundary retained sizes; transient rollback journals and intermediate
candidate sidecars are not a measured peak-disk bound.

The separate real-process acceptance matrix includes fresh PTY/ConPTY setup,
process launch, loading, and first terminal output containing a data row:

| Rows | Launch to painted first page | Cached cursor move and paint | Cancellation acknowledgement |
| ---: | ---: | ---: | ---: |
| 100K | 786.8 ms | 13.2 ms | 30.3 ms |
| 1M | 54.5 ms | 12.4 ms | 31.5 ms |
| 10M | 55.6 ms | 14.8 ms | 26.2 ms |

The 100K launch outlier is retained in the report. Other local runs ranged from
about 48 ms to 298 ms at that size; this variance is why these figures are not
p95 acceptance results. Cancellation acknowledgement measures Esc to the painted
cancellation message during a long recursive SQL operation. Regression tests
separately interrupt an exact-count scan and each promotion phase; the table does
not establish large-backup cancellation latency. Each terminal run also asserts
F6 without promotion, one workspace across two edits, undo/redo, session cleanup,
and unchanged source checksums.

Reproduce with no concurrent build:

```powershell
cargo build --locked --release --bin dataman --example profile_open
python scripts/measure_open.py target/release/examples/profile_open.exe
python scripts/terminal_acceptance.py target/release/dataman.exe 100000
python scripts/terminal_acceptance.py target/release/dataman.exe 1000000
python scripts/terminal_acceptance.py target/release/dataman.exe 10000000
```

On Linux/macOS, omit `.exe`. The scripts use Python's standard library only.
The local checker runs the 100K terminal script across all shapes. Cross-platform results,
cold-cache and percentile targets, wide/skewed datasets, terminal peak RSS,
transient peak disk, and large-promotion cancellation measurements remain open.
Three-platform functional sign-off remains required; these diagnostic gaps do not block the MVP. No VisiData advantage is claimed.

## Historical observations before copy-on-first-write

After the identity migration, a local Windows run on 2026-11-08 with Rust 1.94
measured the following means for the 100,000-row, three-column navigation
fixture. Each estimate uses ten Criterion samples, a 0.1-second warmup, and a
0.5-second requested measurement period. No other build ran during measurement.

| Measurement | Mean | Criterion estimate interval |
| --- | ---: | ---: |
| Open to first usable page | 116.5 ms | 96.3–141.3 ms |
| Adjacent keyset page with lazy identity mapping | 1.35 ms | 1.32–1.39 ms |
| Cached sorted page with lazy identity mapping | 1.40 ms | 1.39–1.41 ms |

The keyset measurement excludes the preceding page fetch. The cached sorted
measurement excludes initial position-map construction. Startup includes all
database work described above, including the eager private copy and exact count.
The release-mode cached terminal rendering test separately measured an average
of 249 microseconds per frame. These are preliminary warm-cache observations,
not p95 figures, memory acceptance results, or comparisons with VisiData. Peak
RSS, cancellation latency, and the 10-million-row diagnostic matrix remain
unverified for this migration.

Reproduce the identity and startup measurements with:

```powershell
$env:DATAMAN_BENCH_ROWS='100000'
cargo bench --locked --bench benchmark_navigation -- 'startup|identity_' --warm-up-time 0.1 --measurement-time 0.5 --noplot
```

Before the generic identity migration, one local Windows run on 2026-11-08
with Rust 1.94 measured the
`startup/open_to_first_usable_page` path at **69.3 ms** for 100,000 rows
(Criterion estimate interval 57.4–81.0 ms, ten samples). The fixture has three
columns: an integer primary key, text with 1,000 repeated values, and an integer
amount. This includes the private copy and exact row count. Filesystem caches
were not flushed; peak RSS and temporary disk were not measured. This is a
preliminary observation, not a performance gate result.

The following measurements predate the workbench source-isolation changes.
They are historical comparisons with earlier Dataman code, not current release
measurements or comparisons with VisiData. Those historical startup paths included eager private copies; the full P0
opening and identity gate remains incomplete.

Short local measurements on the development host, using the unchanged size
optimization settings:

| Measurement | Former pattern | New path |
| --- | ---: | ---: |
| Deep sorted page, 100,000 rows | 92.7 ms | 72.5 µs |
| Deep sorted page, 1,000,000 rows | 2.31 s | 69.7 µs |
| Export peak RSS, 100,000 rows × 16 text columns | 183.8 MiB | approximately 6 MiB |

The one-million-row legacy query had substantial timing variance (approximately
1.7–2.9 seconds). Rebuilding two ordering maps for 100,000 rows took approximately
300 ms total. These are illustrative measurements, not CI thresholds or promises
for other machines. Export RSS includes fixture creation; it compares identical
64-byte cell contents using separate processes. Export completion includes an
output-file sync, so filesystem conditions affect its wall-clock time.

Regression tests enforce the structural guarantees instead: one transaction per
bulk operation, no SQL for unchanged pages or rendering, shared cached cells for
layout changes, rollback on injected failures and cancellation, indexed paging,
selection beyond one million rows, 64-bit IDs, and safe output replacement.
The common README workflows were also exercised through a pseudo-terminal with
CSV/TSV/SQLite outputs checked afterward.

Programmatic callers can continue using synchronous `Database` methods, or use
`Controller::execute(Command, inputs)` without a terminal. `Controller::run`
consumes the controller to transfer ownership to its worker; `run_paths` also
performs initial loading there. The terminal renderer itself is internal and
accepts only immutable cached state.
