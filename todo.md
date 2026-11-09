# Dataman Roadmap — Competing with VisiData

## P0 — Correctness and Data Safety
- [x] Open external SQLite databases read-only by default.
- [x] Never modify or delete user tables during initialization.
- [x] Store internal metadata separately from user data.
- [x] Support tables without an integer `id` column.
- [x] Implement generic row identity using primary keys or stable internal identifiers.
- [ ] Add CSV type inference for integers, floats, booleans, dates, and nulls.
- [x] Ensure mutations are transactional and safely recoverable.
- [x] Add regression tests for data integrity and undo/redo.
- [x] Implement transitive invalidation of derived sheets.

## P1 — File Format Support
- [ ] Support JSON and JSONL.
- [ ] Support Parquet.
- [ ] Support XLSX.
- [ ] Support compressed CSV and TSV.
- [ ] Support reading data from stdin.
- [ ] Support writing data to stdout.
- [ ] Implement automatic file format detection.
- [ ] Support exporting to all major input formats.

## P1 — SQL and Data Exploration
- [ ] Display SQL SELECT results as interactive sheets.
- [ ] Add SQL query history and saved queries.
- [ ] Add SQL autocomplete for tables and columns.
- [ ] Support joining and transforming SQL query result sheets.
- [ ] Improve filtering, searching, and sorting workflows.
- [ ] Add comprehensive column profiling and statistics.
- [ ] Support nested JSON exploration.

## P1 — User Experience
- [ ] Implement a searchable command palette.
- [ ] Add contextual help for commands and keybindings.
- [ ] Add autocomplete for command arguments.
- [ ] Improve error messages and validation feedback.
- [ ] Support persistent sessions and workspace restoration.
- [ ] Improve navigation across large tables and multiple sheets.
- [ ] Provide configurable keybindings and themes.
- [ ] Add progress indicators for expensive operations.

## P1 — Automation and Reproducibility
- [ ] Serialize transformation workflows into reusable files.
- [ ] Support headless execution of saved workflows.
- [ ] Add CLI arguments for input, output, and transformation scripts.
- [ ] Track dependencies between derived sheets.
- [ ] Automatically invalidate downstream derived sheets.
- [ ] Support deterministic workflow replay.
- [ ] Allow exporting interactive operations as executable scripts.
- [ ] Provide meaningful exit codes and errors for automation.

## P2 — Performance
- [ ] Benchmark Dataman against VisiData.
- [ ] Test datasets containing 100K, 1M, and 10M rows.
- [ ] Measure startup time and time to first visible row.
- [ ] Measure scrolling latency, sorting speed, and memory consumption.
- [ ] Replace unnecessary full-table ordering materialization.
- [ ] Implement indexed keyset pagination where applicable.
- [ ] Optimize expensive aggregations and joins.
- [ ] Minimize unnecessary database queries during navigation.
- [ ] Add automated performance regression benchmarks.
- [ ] Establish measurable performance targets against VisiData.

## P2 — Extensibility
- [ ] Design an extensible command system.
- [ ] Support user-defined transformations.
- [ ] Support reusable SQL expressions.
- [ ] Introduce a plugin architecture.
- [ ] Provide stable APIs for external integrations.

## P2 — Visualization
- [ ] Add terminal-based histograms.
- [ ] Add distribution and frequency visualizations.
- [ ] Add interactive column profiling.
- [ ] Support basic scatter plots and charts.

## P2 — Distribution and Adoption
- [ ] Publish prebuilt binaries for Linux, macOS, and Windows.
- [ ] Provide installation through Cargo and popular package managers.
- [ ] Write a comprehensive README with screenshots and examples.
- [ ] Create a quick-start tutorial.
- [ ] Document keyboard shortcuts and workflows.
- [ ] Publish reproducible performance benchmarks against VisiData.
- [ ] Set up automated releases and CI.
- [ ] Collect feedback from real users.

## Competitive Differentiation
- [ ] Make Dataman a fast, SQL-native terminal data workbench.
- [ ] Deliver excellent performance on large CSV and Parquet datasets.
- [ ] Make SQL querying a first-class interactive experience.
- [ ] Allow seamless transitions between interactive exploration and automated pipelines.
- [ ] Prioritize predictable memory consumption and responsive navigation.
- [ ] Focus on superior workflows rather than complete VisiData feature parity.

## Development Order
1. Fix data safety and correctness issues.
2. Implement generic row identity and type inference.
3. Add stdin/stdout, JSONL, and Parquet support.
4. Make SQL query results interactive.
5. Improve command discovery and usability.
6. Implement persistent, executable transformation workflows.
7. Benchmark and optimize large-dataset performance.
8. Publish binaries, documentation, and benchmarks.
9. Add extensibility and visualization based on user demand.

## Workbench milestone acceptance status (2026-11-08)

See [implementation status and release gates](docs/implementation-status.md) for
implemented foundations, partial capabilities, and remaining milestones. The
items above remain a capability inventory; an unchecked item does not erase
existing partial support. Public SQLite releases remain blocked until the full
P0 schema/identity and source-preservation matrix passes.

The initial safety slice is implemented and regression-tested. Integer-key
first-page reads bypass full ordering maps, and adjacent pages use keysets.
Derived-sheet invalidation and refresh now traverse dependency chains, including
undo/redo and source deletion. These remain partial roadmap capabilities because
portable dependency references and workflow persistence are still pending.

Generic runtime identities are implemented. Copy-on-first-write and deferred counts are implemented. Milestone 1 remains incomplete until the full cross-platform terminal/performance acceptance gate passes.
Milestones 2?5 retain their acceptance gates and are not marked complete.
Plugin APIs, custom-command extensions, and charts are deferred until the core
workflows and performance advantage are proven.

The row-identity migration now carries typed handles through paging, selection,
editing, deletion, undo/redo, provenance, and cell-edit replay. User `id` values
remain ordinary data, row positions appear in a gutter, and explicit SQL changes
reset affected row handles. The source-preservation matrix covers duplicate/text
IDs, composite keys, WITHOUT ROWID, and snapshots. See
[the identity contract](docs/row-identity.md) for API and lifecycle details.
