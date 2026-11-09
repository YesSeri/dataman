# Runtime sheet, column, and row identities

The data model exposes opaque `SheetId`, `ColumnId`, and `RowId` types. Callers
can copy, compare, and hash them, but cannot construct identifiers from integers.
A row handle is neither a visible position nor a user column value.

## Public API

```rust
let sheet = database.open_sheet("orders")?;
let page = database.fetch_page(sheet, 100, 0, "")?;
let amount = database.column_id(sheet, "amount")?;
if let Some(row) = page.row_ids.first().copied() {
    database.edit_row(sheet, row, amount, "42")?;
}
```

`Page` contains column metadata, cell values, an aligned vector of row
handles, and `has_more`. The continuation sentinel receives no row handle. Column metadata includes its `ColumnId`, name, declared SQLite type,
and whether the underlying column is writable. `cell_value` reads through the
same identity abstraction. Lookup names follow SQLite's case-insensitive naming.

The retained `get`, `get_cell`, and `update_cell` methods are compatibility
helpers. Integer-key helpers require a verified unique integer user `id`, or an
accessible native rowid when there is no `id` column. They do not provide the
lifetime guarantees of opaque handles. New integrations should use the API above.
The public `execute_batch` method uses the same guarded SQL boundary as the TUI.

## Storage and lifetime

- Ordinary tables use an unshadowed SQLite rowid alias. Tables without rowids
  use ordered non-null primary-key tuples with their native values and
  collations. Duplicate user IDs and user columns named `id` are ordinary data.
- Mappings are allocated lazily for fetched or selected rows. They live in a
  named temporary SQLite sidecar, with a bounded page cache, rather than a Rust
  map containing every source row. Arbitrary sorted-position maps also live
  there and contain only native locators and positions.
  The sidecar uses a disk rollback journal without durable fsync; it is discarded
  at session exit or after a crash. Live rollback and cancellation remain atomic.
- Sheet and column handles survive built-in renames. Row handles survive
  sorting, column layout changes, ordinary edits, key edits, and undo/redo.
  Creating a filtered copy retains the source handles; the new sheet has its
  own independent handles. A deleted row is inaccessible until undo restores it.
  Expired deleted objects cannot acquire new meanings when names are reused.
- Handles belong to one session and one sheet. Cross-sheet, cross-session,
  deleted, and invalidated handles fail explicitly. Runtime tokens are never
  recycled within a session. They are not portable workflow keys.
- After promotion, session-only TEMP triggers track native key changes, including foreign-key
  cascades and trigger-driven replacement of other rows. The callback writes
  only the session sidecar and participates in the operation's transaction.
  SQLite constraints, foreign keys, cancellation, and mapping updates roll back
  together on failure.

The terminal uses a `#` gutter for one-based visible positions. Gutter numbers
change when sorting changes. They are excluded from data and exports. A user
`id` column can be edited, hidden, moved, renamed, grouped, joined, and exported;
normal SQLite constraints still apply. Primary-key column deletion is not
supported.

## Snapshots, SQL, and refresh

Views, virtual tables, and sources without a usable locator open as explicitly
named snapshot sheets. Opening the same source again reuses its session copy.
These are frozen tabular copies: edits do not write through to the source view.
Normal snapshots use native SQLite rowids. When every rowid alias is shadowed,
the snapshot has a collision-free private non-null unique locator. That column
and its owned index are omitted from exports. SQL inserts into this unusual
storage must satisfy its private locator constraints; modifying or removing
the locator through arbitrary SQL is rejected.

Successful explicit SQL mutations clear affected row mappings and selections.
Trigger and foreign-key side effects are included by SQLite authorization
metadata. Schema changes conservatively invalidate all runtime handles, and
the next page discovers fresh metadata. Failed or cancelled batches preserve
the previous mappings and undo history. SQL remains an undo boundary.

SQL cannot attach external databases, alter connection settings, edit session
records, or manipulate owned undo/provenance tables. Raw SQL structural changes
on sheets with managed internal columns or indexes are restricted; use the
built-in rename/delete commands, which update ownership and identities together.

Refreshing a derived sheet retains its sheet identity, replaces its row
identities, clears its old row selections, and removes undo entries targeting
replaced rows. Source edit history is retained. A failed refresh keeps the prior
result and handles. Frequency drill-down rejects stale provenance until refresh.
Cell-edit replay follows the recorded row and column handles, rather than the
old cursor position, and rejects rows invalidated by SQL or refresh.

Some uncommon operations fail deliberately and atomically: a rename that would
hide the final native rowid alias, deriving a column that shadows the active
locator, and an AFTER trigger that relocates the edited row beyond SQLite's
RETURNING locator. These failures preserve the prior data and mappings.

Workspace persistence and portable workflow references remain separate roadmap
work. SQLite/CSV exports contain data, not session row handles or undo history.

## Source snapshots and promotion

`Database::storage_state()` distinguishes `ReadOnlySource` and `PrivateWorkspace`.
`open_sheet`, value-only compatibility `get`, and mutation APIs require mutable
database access because they may replace the active connection. Handle-based
page and value reads retain shared access.
Compatible source tables use operation-scoped read transactions covering source
reads, ordering maps, and identity allocation. Browsing installs no mutation
trackers on the source. Counts are explicit (`count_rows`) or inferred from EOF;
`row_count_state` is cache-only and returns `Unknown { known_at_least }` or `Exact`.

The first writable operation uses bounded backup steps into an unpublished
candidate. It reattaches the existing session sidecar, recreates the TEMP catalog,
registers functions and cancellation, and installs tracking before publication.
Sheet, column, and row handles, selections, and layout survive this replacement.
Failed promotion leaves the source session usable. A failed first mutation leaves
a valid private workspace with that mutation rolled back. Further writes reuse it.
Explicit SQL, including the existing SELECT execution service, promotes; read-only
interactive SQL classification is deferred to Milestone 2.

The retained connection's `data_version`, schema version, and filesystem identity
are checked at operation snapshot boundaries and before candidate publication.
External commits discard results and invalidate handles, ordering maps, selections,
count caches, and replay targets. Stale edits fail with a review-and-retry message;
rediscovery allocates fresh handles rather than assigning old tokens new meanings.
A replaced or inaccessible source path requires reopening. The terminal retains
its last cached page when further data access fails. Exports reject both the
original source path and hardlinks to its retained file identity after promotion.
