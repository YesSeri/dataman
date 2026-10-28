# TODO

## Priority: VisiData workflow

- [ ] Add undo and redo for data-changing commands (edits, transforms, deletes, and renames).
- [ ] Add a command history that records operations and their inputs; make history reviewable and replayable.
- [ ] Add row selection and bulk operations for selected rows.
- [ ] Add horizontal scrolling for tables wider than the terminal.
- [ ] Add commands to hide, show, and reorder columns.
- [ ] Improve status and error messages so command results and failures are visible in the TUI.
- [ ] Add a column summary view with useful statistics for each column.
- [ ] Add frequency tables that group rows by column values and summarize groups.
- [ ] Add support for opening and saving TSV and SQLite files through a common path-based input workflow.
- [ ] Add joins between tables, starting with a table-of-tables workflow.

## Less priority

- [ ] Consider whether regex execution needs an `RwLock`.
- [ ] Simplify the controller/input pattern using Ratatui guidance.
- [ ] Clarify and support single versus multiple input files.

## Completed

- [x] Allow viewing more than the initial ~50 rows by paging with an offset as the cursor moves past the visible height.
