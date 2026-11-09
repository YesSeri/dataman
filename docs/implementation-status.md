# Workbench roadmap: implementation status

Public SQLite releases remain blocked until the complete P0 gate passes. The
copy-on-first-write and deferred-count implementation is present, with local
Windows regression and terminal evidence. Cross-platform CI results and repeated
performance acceptance measurements are still required. No advantage over
VisiData is claimed.

## Implemented P0 behavior

- Compatible SQLite tables browse directly from a read-only main database.
  CSV/TSV and in-memory sessions start as private workspaces. Browse-only paging,
  selection, search, sorting, counting, and exports do not create a full workspace
  copy. Session identity and sort maps can grow independently of source data.
- Source operations use scoped read transactions covering reads, map construction,
  and identity bookkeeping. Transaction ownership uses explicit operation depth.
  The retained connection's data/schema versions and filesystem identity detect
  external commits, schema changes, inaccessible files, and path replacement.
  Commits invalidate stale handles, selections, count caches, and replay targets;
  pending edits fail with a review-and-retry message. Path replacement or loss of
  access requires reopening; the terminal keeps its last cached display.
- The first writable operation uses a bounded, cancellable whole-database backup
  into an unpublished private candidate. Functions, cancellation, session storage,
  TEMP catalog, and mutation trackers initialize before publication. Handles and
  layout survive promotion. Failed promotion retains the source session; failed
  first mutations retain a valid workspace with the mutation rolled back.
- Edits, schema changes, conversions, materialized transformations, named
  snapshots, and explicit SQL use promotion. Explicit SQL behavior is unchanged:
  even SELECT execution prepares writable storage. Interactive SQL classification
  belongs to Milestone 2. Metadata writes and sort maps do not promote.
- Page fetching reads a continuation sentinel without allocating its identity.
  Row counts begin unknown and are inferred only from justified EOF ranges or
  an explicit count. Rendering, catalog population, startup, and cursor movement
  do not count every source table. F6 (`count_rows`) counts on the worker, supports
  Esc cancellation, and creates no undo/replay entry. No background count runs.
- Original source paths and hardlinks remain protected from exports after
  promotion. Source schema/user objects are preserved regardless of names.
  Output replacement, transactional rollback, generic identities, undo/redo,
  foreign-key/trigger tracking, and cached rendering remain in place.

## Verification

Local Windows checks include formatting, strict Clippy, library/integration tests,
all-target benchmark smoke checks at 1,000 rows, and optimized executable/example
builds. The final local suite passed 197 tests (179 library and 18 integration tests),
with one intentionally ignored manual render benchmark.
The new regressions cover promotion phases, SQLite disk-full backup failure,
candidate initialization failure, failed first edits, one-copy reuse, native
locator shapes, restored catalog handles/functions/trackers, external WAL commits,
schema changes, rollback-journal contention, cancelled counts, unknown totals,
empty/final pages, arbitrary-offset EOF, and source-path/hardlink protection.
A structural authorizer test rejects COUNT during initial compatible display and
asserts no workspace or mutation trackers were created.

`scripts/terminal_acceptance.py` runs the real release executable through PTY or
Windows ConPTY. Local 100K, 1M, and 10M runs passed first-page painting, F6 without
promotion, cached navigation, one workspace across two edits, undo/redo,
cancellation, session cleanup, and source checksum preservation. Fixtures are
created outside launch timing. See [performance.md](performance.md) for measured
costs and their limits, and [tutorial.md](tutorial.md) for the user flow.

The CI matrix runs formatting, strict Clippy, all test targets, release builds,
and the 100K-row terminal acceptance script on Linux, macOS, and Windows. It does
not publish binaries. These CI jobs have not been dispatched or observed from
this local session. Linux/macOS terminal and source-replacement acceptance remain
unverified here; WSL is available but has no Rust toolchain installed.

## Remaining gates and milestones

1. Obtain passing Linux/macOS/Windows CI, including Unix source replacement and
   real terminal acceptance. Repeat the performance matrix under controlled
   conditions and assess the P0 targets: cold-cache opening, wide/skewed fixtures,
   p95 timings, peak temporary disk, and cancellation during large promotion and
   counts remain open. Current single-run timings do not establish those gates.
2. Add interactive read-only SQL classification/results, multiline editing,
   completion, persistent query history, saved queries, and command palette/help.
3. Extend streaming input/output through stdin/stdout, gzip, JSON/JSONL, then
   Arrow/Parquet and XLSX. Preserve raw values and conversion diagnostics.
4. Introduce typed portable operations, workflow recording/replay, CLI bindings,
   and atomic persistent workspaces. Session handles remain nonportable.
5. Run pinned Dataman/VisiData comparisons across the full 100K/1M/10M matrix and
   complete the broader workflow/tutorial/distribution gate before comparative
   claims or public releases. Plugin APIs and custom extensions remain deferred.
