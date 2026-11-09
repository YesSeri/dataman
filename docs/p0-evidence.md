# P0 testing evidence

The implementation is on `p0-testing`. Functional source revision:
`102f4386a62946252186e10aa8c4dd1050f621e5` (followed by removal of CI and evidence updates).
There is **no CI pipeline** for this early MVP, at the user's request. The earlier
push did start [a workflow run](https://github.com/YesSeri/dataman/actions/runs/42695249032),
which completed with failure before CI was removed. It is not sign-off evidence
and no further CI runs are requested. Linux/macOS functional results remain
pending manual runs; local Windows results do not substitute for them. No release
was published.

Local records before that implementation commit use base revision
`673000eb40db11b8d117566c599f9b66a7bf2ce5` plus working-tree changes.

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
