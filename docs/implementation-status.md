# Workbench roadmap: implementation status

SQLite P0 sign-off requires passing functional checks on Windows, Linux, and
macOS, with no known source-safety, identity, rollback, or cancellation correctness
failures. Performance measurements are non-blocking diagnostics. Interactive SQL
results, additional formats, CSV type inference, and persistent workspaces remain
deferred. No advantage over VisiData is claimed.

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

## Verification and evidence

The implementation branch extends the existing identity/paging matrix rather
than duplicating it. Deterministic barriers coordinate cancellation after a
successful 128-page backup step, cancellation after an exact-count scan begins,
and a WAL commit after the promotion snapshot starts. Regressions require no
published candidate or stale target, usable surviving connections/identities,
and successful retry. A controller count test also checks selection, navigation,
unknown totals, and absence of undo/replay entries. Existing fault injection
covers SQLite disk-full, contention, and initialization failure immediately before
publication. Unix tests cover both replacement and unlink; Windows tests require
sharing-denied replacement/removal to preserve browsing and private editing.

The terminal harness decodes incremental PTY/ConPTY output into current screen
cells. It checks an idle first page with real rows and unknown total, exact F6
count without promotion, cached cursor movement, stale-edit review/retry and
identity refresh, two edits in one workspace, and undo/redo against both the
painted data row and SQLite workspace. It requires exit status zero, unchanged
source checksums (apart from its explicit external writer), and complete session
cleanup. A 120-second operation watchdog and 10-second cancellation watchdog
identify hangs; they are not performance targets. Failed checks retain transcript,
current screen, timeline, exit status, and session-file inventory without copying
database files. Every platform replays its captured application output in 1-, 7-,
and 127-byte fragments, including splits inside UTF-8/escape sequences.

Run `python scripts/check_p0.py` locally. It includes formatting, strict Clippy,
all test targets with 1,000-row benchmark smoke checks, Python decoder/fixture
tests, optimized executable/example builds, and 100K-row narrow/wide/skewed
terminal acceptance. Logs, acceptance JSONL, terminal captures and detailed
failures remain in `artifacts/`. Environment records include the exact SHA,
Rust/Cargo/Python versions and platform. No release is published.

There is **no CI pipeline** for this early MVP, at the user's request. Functional
checks on Linux/macOS remain manual follow-up work; local Windows evidence does
not establish those results. Current outcomes are recorded in
[P0 evidence](p0-evidence.md).

## Remaining gates and milestones

1. Obtain local Windows/Linux/macOS functional results on the implementation commit.
   Source-safety, identity, rollback and cancellation failures block sign-off.
   Performance measurements and slow outliers create follow-up work; they do not
   block this MVP. See [performance.md](performance.md) for diagnostics, skips and
   reproduction commands.
2. Add interactive read-only SQL classification/results, multiline editing,
   completion, persistent query history, saved queries, and command palette/help.
3. Extend streaming input/output through stdin/stdout, gzip, JSON/JSONL, then
   Arrow/Parquet and XLSX. Add CSV type inference with raw-value preservation and
   conversion diagnostics in this deferred import/type milestone.
4. Introduce typed portable operations, workflow recording/replay, CLI bindings,
   and atomic persistent workspaces. Session handles remain nonportable.
5. Run pinned Dataman/VisiData comparisons and complete the broader workflow,
   tutorial and distribution checks before comparative claims. Plugin APIs and
   custom extensions remain deferred.
