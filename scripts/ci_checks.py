"""Run independent checks to completion, retaining evidence on every platform."""
import os
from pathlib import Path
import subprocess
import sys

from diagnostics import SHAPES, emit, metadata


def main():
    root = Path("artifacts")
    root.mkdir(exist_ok=True)
    emit(dict(kind="environment", **metadata(root)), root / "environment.jsonl")
    commands = [
        ("format", ["cargo", "fmt", "--all", "--", "--check"]),
        ("clippy", ["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"]),
        ("tests", ["cargo", "test", "--locked", "--all-targets"]),
        ("python-tests", [sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"]),
        ("release", ["cargo", "build", "--locked", "--release", "--bin", "dataman", "--example", "profile_open"]),
    ]
    binary = "target/release/dataman" + (".exe" if os.name == "nt" else "")
    for shape in SHAPES:
        commands.append(("terminal-" + shape, [sys.executable, "scripts/terminal_acceptance.py", binary, "100000", "--shape", shape,
                                              "--jsonl", str(root / "acceptance.jsonl"), "--artifacts", str(root / "terminal")]))
    failed = False
    env = dict(os.environ, DATAMAN_BENCH_ROWS="1000")
    for name, argv in commands:
        print("Running", name, flush=True)
        with open(root / (name + ".log"), "w", encoding="utf-8") as log:
            process = subprocess.run(argv, stdout=log, stderr=subprocess.STDOUT, env=env)
        emit(dict(kind="check", check=name, exit_status=process.returncode), root / "checks.jsonl")
        print((root / (name + ".log")).read_text(encoding="utf-8"), flush=True)
        failed |= process.returncode != 0
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
