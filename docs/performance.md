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
SQLite's progress handler checks it during SQL, and Rust import, export, regex,
and derivation loops check it between records or batches. Transactions roll back
on failure or cancellation. Progress interruption is disabled during the final
commit/rollback boundary. A cancellation after that boundary does not undo an
already completed edit. Errors refreshing its page are reported separately.

Imported datasets live in unique temporary directories, with the connection
closed before cleanup. Existing SQLite inputs retain their in-place editing
behavior. CSV/TSV export and SQLite backup use a temporary sibling output and
replace the destination after successful completion. The active SQLite source
cannot be replaced by an export.

## Caching and memory

- Schemas, table names, and row counts are cached. Cell value changes retain
  schema/count caches and preserve an existing ID ordering map when triggers and
  foreign-key cascades cannot affect other rows or tables.
- One temporary ordering table maps indexed positions to row IDs and storage
  keys. Sorting builds this map once; navigation uses position ranges and direct
  storage-key lookups. This also handles derived tables without an ID index.
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
Editable tables still need a unique integer `id`, as before. Row IDs now use the
full signed SQLite integer range. Row counts/page offsets retain their existing
`u32` interface.

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

## Observations from this implementation

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
