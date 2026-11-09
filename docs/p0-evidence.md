# P0 testing evidence

Functional P0 sign-off is pending. The implementation is on `p0-testing`.
The tested source revision and CI run URL will be recorded after the branch push.
The base revision is `673000eb40db11b8d117566c599f9b66a7bf2ce5`; local records
made before committing include working-tree changes and are not clean-commit CI
proof. No release is published.

The local Windows toolchain is Rust/Cargo 1.94.0, Python 3.11.4, x86_64 MSVC,
Windows 10 build 19045, Intel family 6 model 158, 32 GiB RAM, NTFS.
Initial sandboxed all-target builds reported disk-full errors despite host free
space. An unsandboxed retry passed the Rust suite, benchmark smoke checks,
strict Clippy, formatting, Python tests and release build. The final full local
rerun includes the added coordinated controller count test.

Initial harness failures were retained while fixing source refresh sequencing
and recognizing the first painted data row when wide columns hide the gutter.
These were harness defects; SQLite workspace inspection confirmed successful
mutation. Initial diagnostic sampling also exposed directory deletion racing
with recursive enumeration; the sampler now tolerates vanished directories and
records sampling errors. Initial runs are not the final measurement matrix.

All performance results are warm/cache-uncontrolled and non-blocking. P95 and
verified cold-cache targets remain unassessed. Final measurements and follow-up
work are appended after the sequential diagnostic run.
