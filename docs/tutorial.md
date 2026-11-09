# Browse and edit a SQLite source

Run `dataman source.sqlite`. A compatible table opens directly from the read-only
source. The first page does not wait for a database copy or a full row count.
The `#` gutter contains visible row positions; user columns remain data.

1. Use the arrow keys to browse. Press `s` to sort the selected column, `/` to
   search, and Space to select a row. These operations can allocate session
   identity or sort maps, but do not copy the source database. A sort-map build
   can be expensive; Esc cancels it while the cached page remains visible.
2. The status line initially says `total rows: unknown`. Press **F6** to count
   the active sheet. Esc cancels a running count. Paging to a justified EOF also
   establishes an exact total. No background count starts automatically.
3. Move to a cell, press `e`, type a replacement, and press Enter. The first
   write prepares a private workspace. The worker reports preparation, copying,
   and initialization phases. Later writes reuse that workspace. Views or
   sources without a usable native locator prepare a named editable snapshot
   when opened, so they can require this work before their first page.
4. Press Ctrl+Z to undo and Ctrl+Y to redo. Row handles survive workspace
   preparation, sorting, and key changes. A cancelled or failed mutation rolls
   back; successful workspace preparation can remain available for retry.
5. Press Ctrl+S and enter a different `.sqlite`, `.csv`, or `.tsv` destination.
   The original SQLite file cannot be overwritten, including through a hardlink.
   Ctrl+C exits and removes session storage. Unsaved private edits are temporary.

Another application may commit to the source while you browse. Dataman rejects
pending edits with `Source changed; review and retry`, invalidates old identities
and selections, and rediscovers the data for the next operation. Review the new
page before editing again. If the path is replaced or becomes inaccessible,
reopen the file; the last cached terminal page remains visible.

Explicit SQL currently prepares writable storage even for SELECT statements and
forms an undo boundary. Interactive read-only SQL results are a later milestone.
Public SQLite releases remain blocked on the complete P0 acceptance gate in
[implementation-status.md](implementation-status.md).
