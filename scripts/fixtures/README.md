`windows-conpty.vt` is real Windows ConPTY output from Dataman using the
generated 100K narrow fixture. It includes initial painting and incremental
redraws, cursor queries, styling, erasure, and an external-edit error.

Every acceptance run also retains `terminal-capture.vt` and checks replay with
1-, 7-, and 127-byte fragments against whole-stream decoding. CI therefore
tests captured application output from both Unix PTY and Windows ConPTY.
No SQLite database files belong in this directory or CI artifacts.
