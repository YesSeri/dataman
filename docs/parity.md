# Core parity automation

VisiData compatibility is now the default interaction profile. The default binds only the small supported subset: arrows/hjkl, `q`/Ctrl+C, `gt` (invert selection), and `gu` (clear selection). Additional VisiData interactions are explicit gaps in the backlog. To keep the previous interface:

```sh
dataman --keymap dataman input.csv
```

The original built-in bindings are preserved in `assets/keybindings-dataman.toml`. `keybindings.toml` now contains user overrides, applied after either profile. A sequence is written as `"gt" = "invert_selection"`; context overrides use `[contexts.sheet_alias]`. Escape clears a pending sequence and cancels prompts. The dispatcher and Unicode prompt editor are shared by the TUI and terminal-independent sessions. Render caches and worker cancellation remain separate.

## Contract and reference

Core scope is navigation, sheets, columns, selection/search, editing, logical types/errors, transformations, analysis, combining tables, local CSV/TSV/SQLite, and persistent replay. Formats beyond these, remote services, plotting, Python plugins, bundled apps, and experiments are deferred. This milestone does not claim the complete scope is implemented.

`parity/backlog.json` schema 1 is the task contract. Implementation state and behavioral/interaction verification state are independent. Existing implementations are **unverified** until acceptance evidence exists. Only foundation tasks and `selection-invert` are ready. Other tasks carry blocking design decisions and acceptance placeholders; they cannot enter unattended execution. The requested separate feature audit/upstream inventory were absent from this checkout. The existing README roadmap, todo.md, and source inventory are the available seed inputs, preserved in place.

The reference is the [upstream v3.5 release](https://github.com/saulpw/visidata/releases/tag/v3.5), resolved to `f730c389e601dc156c42eb80acb1302d7d268f55`. `parity/reference-lock.json` pins its source archive checksum and Python 3.10.12. Runtime dependencies have hash-checked locks. Provision explicitly with `just parity-setup`; the interpreter is always `.parity/venv/bin/python`. An installed v3.0.2 is never discovered or used. The stdlib venv bootstrap uses the Python distribution's pip/setuptools; the reference runtime itself is locked and verified.

The adapter executes VisiData's own `replayOne`, including `open-file`; it only inspects sheet data afterwards. This follows upstream's [command-log/golden-output testing approach](https://github.com/saulpw/visidata/blob/v3.5/dev/TESTING.md). Invert selection is upstream `stoggle-rows`, bound to `gt` in [selection.py](https://github.com/saulpw/visidata/blob/f730c389e601dc156c42eb80acb1302d7d268f55/visidata/selection.py#L215).

## Workflows and state

```sh
dataman input.csv --batch --play workflow.jsonl --dump-state result.json
dataman --list-commands --json
dataman --keymap dataman input.csv --record workflow.jsonl
```

Native JSONL starts with `{"kind":"header","schema_version":1}`. Subsequent records are `command` (`name`, string `inputs`), `context` (`sheet`, zero-based `row`/`column`), or `key` (`key` token). Commands use stable registry names, not enum display text. Empty strings are valid explicit edit values. Unknown names/fields, invalid argument counts, missing sheets and bad versions fail. Replay stops at the first failure. Diagnostics contain a one-based step, phase, primary message and separate subsequent snapshot error. Batch never initializes a terminal.

`Session` exposes synchronous load, execute, context, key, snapshot, streaming snapshot output and complete workflow output. Execution completion is synchronous; no polling is needed. TUI key recording is append-only and flushed per key, independent of the old bounded history. It includes navigation, prefixes, prompt input, selection and key-marker keystrokes. Replay requires the same input sources and profile. External editor use is explicitly unavailable in batch replay. Keep a native workflow for reproducibility; TUI keys pressed during busy cancellation are timing-sensitive and are not a deterministic command contract.

Snapshots have schema version 1, sheet aliases/order, active sheet, physical column types/order/visibility, typed cells, cursor, sort, selection, keys, derived source aliases, stale flags, prompt, pending keys and structured errors. Row references use the session's stable row IDs, distinguishing identical values. Null, empty text, integer, float and blob values remain distinct. Persistent logical conversion errors and stable sheet IDs remain explicit backlog tasks. Full output streams rows in chunks of 1,024; it does not use the paginated render snapshot.

`python3 tools/vdj.py import input.vdj output.jsonl` and `export` implement a strict registry subset (`stoggle-rows`, `unselect-rows`). Unsupported longnames, arguments, indexed contexts and native key records fail. This is not full VDJ compatibility. In particular, upstream `t` advances the cursor, so it is deliberately not mapped to dataman's current toggle-row semantics.

## Verification and contract maintenance

- `just parity-test`: Rust regression and offline runner/comparator tests.
- `just parity-pty`: Linux PTY startup, sequence, prompt, cancellation and exit checks for both profiles. Ratatui TestBackend tests cover rendering with cached state; colors and borders are not parity criteria.
- `just parity-verify`: build and verify immutable reference checkpoints; write `.parity/results.json`.
- `just parity-report`: readable scenario differences and separate behavioral/interaction counts.
- `just parity-reference-update`: explicit, separately reviewed oracle maintenance. Normal verification cannot regenerate references.

Scenarios declare fixtures, native steps, VisiData steps, checkpoints and normalization. The initial data/selection projection omits dataman's synthetic `id` column and normalizes row references to fixture occurrence IDs. Ordered row values and selected flags are compared exactly. There is no implicit sorting, float tolerance, or null/empty coercion. This projection does not claim cursor or prompt parity for existing commands; interaction gaps are reported separately. The invert keyboard cases exercise the same dispatcher used by the terminal.

Fixtures run on temporary copies in both adapters; SQLite sessions also use private backup copies. Reference metadata includes release identity, source checksum, fixture hashes, complete scenario file hash, dependency-lock hash and adapter hash. Mismatches and missing environments are infrastructure failures. Skipped/unsupported cases never pass. `parity/baseline.json` records inherited checks: 148 passed, one ignored, no inherited failures. Local full logs remain under `/tmp` and `.parity`.

Changing contracts, comparator normalization, reference snapshots, task scope, dependencies or runner policy requires maintenance outside implementation runs: edit the contract, explicitly regenerate affected references, run the tooling/regression checks, review the complete diff, and commit a new base before starting another run. Implementation agents are forbidden to perform this maintenance.

## Runner

The runner uses the installed CLI's [non-interactive JSON events and output schemas](https://learn.chatgpt.com/docs/non-interactive-mode), checking required flags first. It inherits the configured model, uses the workspace-write sandbox for implementation and a separate read-only invocation for review. It uses no bypass flags, merges or pushes. Interactive-approval tasks must return blocked. Authentication failure stops the run.

```sh
just parity-fake-run COMMITTED_BASE       # offline integration double
just parity-run COMMITTED_BASE            # explicit paid Codex run
just parity-resume RUN_ID
just parity-status RUN_ID
```

The base must be an explicit committed revision containing these foundations. Foundation prerequisites are checked in a detached worktree of that base. Implementation tasks each receive a detached worktree based on the current automation tip. Original working-tree contents, including dirty changes, are left untouched. Implementation files are staged explicitly after external acceptance/regression checks and separate review approval. The automation branch advances with compare-and-swap ref updates. Commits are `verified_awaiting_review`, which is distinct from merged completion.

`.parity/runs/RUN_ID` is ignored local storage for durable state, prompts, JSONL events, schema-constrained results, expected failures, acceptance evidence, regressions, reviews and partial worktrees. Limits are three implementation tasks, two attempts per task and two hours from initial run creation (including resume time). Foundation verification is prerequisite validation and does not consume implementation task slots. Retry exhaustion blocks that task; independent ready tasks may continue. Timeout/interruption terminates subprocess groups and preserves work. Resume recognizes commits made immediately before a state write and avoids duplicate commits. An exclusive run lock prevents concurrent resume.

Protected paths include `parity/`, `tools/`, `tests/`, manifests, `justfile`, and `.gitignore`. Both the task diff and trusted contract copy are checked; implementation cannot rewrite its own oracle or policy. The policy assumes ordinary local processes with repository permissions, not hostile code in a kernel-level security boundary.
