# Dataman

`dam` is an Excel-like data wrangler in the terminal, inspired by [VisiData](https://www.visidata.org/). This application allows you to manipulate and transform data using regex, logic or SQL. It loads CSV/TSV files into a private temporary SQLite database and uses SQLite to query and transform the data.

## Installation

To install and run the data manager, dataman, follow these steps:

1. **Clone the repository**:
    ```sh
    git clone https://github.com/YesSeri/dataman
    cd dataman
    cargo build --release
    # copy into your path e.g
    cp ./target/release/dataman ~/.local/bin/dam
    # or if you don't want to add to path or can't get it to work
    # ./target/release/dataman
    # cp ./target/release/dataman ./dam
    dam your-data-file.csv
    # or
    # ./dam your-data-file.csv
    ```

## Commonly Used Key Commands

## Supported workflow checklist

This checklist defines Dataman's current common-workflow parity target. CSV and SQLite remain supported; TSV is supported for delimited input and output.

- [x] Open one CSV, TSV, or SQLite/DB file; open multiple CSV/TSV files as separate tables.
- [x] Navigate rows, columns, and tables; sort, search, filter, and inspect metadata.
- [x] Edit cells, derive columns, rename or delete columns/tables, and undo/redo built-in mutations.
- [x] Save the current table as CSV/TSV or the complete dataset as SQLite.
- [x] Create a summary or value frequency result table for the selected column.
- [x] Join using the selected column and a typed `table.column` target; sources remain available.
- [x] Select rows with Space and bulk edit the current column with `B`.
- [x] Navigate wide tables with horizontal scrolling and manage column visibility/order.
- [x] Review and replay supported data and layout commands.

Command history is kept for the current session. SQL and file saves are excluded from replay.

This checklist is the supported workflow target and release gate.

### VisiData workflow roadmap status

- [x] The `M` sheet list opens the highlighted table with Enter, supports Space/`U` selection, `R` rename, `D`/`d` delete, and `[`/`]` navigation order. Batch deletion is one undoable operation.
- [x] Preserve cursor, sort order, and row selection independently for each table while switching; `Ctrl+6` (also `Ctrl+^`) returns to the previously visited table.
- [ ] Stable sheet IDs (derived sheets now track source names and become stale after source edits or SQL changes).
- [ ] Logical type configuration and persistent conversion error records. `#` now makes a strict, undoable integer-derived column and leaves invalid input null.
- [x] `V` creates an all-column Describe sheet with null, invalid, distinct, min/max, sum, and average statistics.
- [x] Refresh stale derived sheets with `F5`; source edits mark dependent frequency, Describe, grouped, pivot, join, and append results stale. Refresh rebuilds the sheet atomically and retains its name.
- Frequency results already include percentages and source metadata; Enter highlights matching source rows, and selected groups propagate their row selection to the source.
- [ ] Grouped-result row provenance, drill-down, and source-selection propagation.
- Grouping/pivot generation is available with `!` row keys, `+` column aggregators, `F3`, and `F4`; pivots reject more than 1,000 generated columns.
- [x] Select tables in the sheet list and press `&` to append them, or choose inner/left/full join for two tables. Mark ordered join keys with `!`; creation is undoable. Source names appear in the sheet list.

Unchecked items above are planned work and are not supported by the current release.

The following are some of the more commonly used key commands in the TUI application:

### Table Navigation
| Key Command          | Action              |
| -------------------- | ------------------- |
| `Ctrl + Right`       | Next Table          |
| `Ctrl + Left`        | Previous Table      |
| `Ctrl + ^`           | Return to Previous Table |
| `Ctrl + 6`           | Return to Previous Table |
| `Right/Left/Up/Down` | Move cursor         |
| `Ctrl + c`           | Quit                |
| `Ctrl + s`           | Save                |
| `Ctrl + z`           | Undo                |
| `Ctrl + y`           | Redo                |
| `M`                  | Show Sheet List |
| `V`                  | Describe All Columns |
| `v`                  | Value Frequencies  |
| `J`                  | Join Tables        |
| `!`                  | Mark/unmark the current column as an ordered join or grouping key |
| `&`                  | Combine selected tables (append, inner, left, or full) |
| `+`                  | Configure count, distinct count, sum, average, min, or max |
| `F3`                 | Group by marked keys |
| `F4`                 | Pivot the current column using marked row keys |
| `Space`              | Toggle row/table selection |
| `B`                  | Bulk edit selected rows in current column |
| `|`                  | Add rows matching a SQLite `WHERE` expression (for example `amount > 20`) |
| `\\`                 | Add rows matching a regex in the current column |
| `d` / `C`            | Delete selected rows (or selected tables in the sheet list) / open copies |
| `U`                  | Clear row/table selection |
| `h` / `o`            | Hide current column / show all columns |
| `[` / `]`            | Move current column left/right; reorder sheets in the sheet list |
| `L`                  | Review history; arrows select, Enter replays |

### Data Transformation

Creates new column unless otherwise specified.

| Key Command | Action                   |
| ----------- | ------------------------ |
| `t`         | Regex Transform          |
| `f`         | Regex Filter             |
| `e`         | Edit a Cell              |
| `m`         | Logic Operation          |
| `s`         | Sort Column              |
| `q`         | SQL Query                |
| `f`         | Regex Filter (new table) |
| `/`         | Exact Search             |

### Table Transformation

| Key Command | Action        |
| ----------- | ------------- |
| `#`         | Text to Int   |
| `$`         | Int to Text   |
| `X`         | Delete Column |
| `D`         | Delete Table  |
| `r`         | Rename Column |
| `R`         | Rename Table  |


### Usage

1. **Simple transformation**: Use key commands to perform operations such as sorting, filtering, and running SQL queries.

2. **Input transformation**: Use the `t` key command to transform data using regex. For example, to extract the first word from a column, use the following regex: `^(\w+)`. Logic operations can also be performed using the `m` key command, e.g., `col1 + col2`, or `col1 > 20`. Combining logic operations with regex filter is powerful. `$` `f` `1`, allows you to filter for logic expressions. We go first to the column we have created from our logic operation then we convert it to text and then only keep the strings that contain the text "1". User friendlyness will improve with time.

Undo and redo are available for built-in data operations during the current session. SQL commands are not undoable and clear the current undo history.
Names beginning with `__dataman_undo_` are reserved for temporary undo data and are not supported for imported tables or columns.

## Session storage and responsiveness

Each CSV/TSV import session uses its own temporary database, removed on normal exit. Save your imported data to keep changes. Existing SQLite files are edited in place.

Database operations run on a worker while the terminal displays cached data. During an operation, you can resize the terminal, move within cached rows, or press Esc to cancel. Additional data commands are rejected while an operation runs; repeated page, table, or sort navigation keeps only the latest pending request. Edits already completed remain applied if you cancel their subsequent page refresh.

Exports are streamed to a temporary output and replace the destination only after success. Saving over the currently open SQLite source is rejected; choose another output path.

Developer benchmarks, architecture details, and measurement instructions are in [docs/performance.md](docs/performance.md).

### Parity automation and interaction profiles

VisiData is now the default keymap, initially with a small verified binding subset.
Use `dataman --keymap dataman input.csv` to retain the bindings documented above.
See [the parity contract and runner guide](docs/parity.md) for pinned VisiData setup,
batch JSONL replay, structured snapshots, coverage, and isolated local agent runs.
