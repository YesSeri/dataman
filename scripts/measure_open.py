"""Sequential non-blocking diagnostics; build binaries before running this tool."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from diagnostics import SHAPES, Sampler, checksum, emit, fixture, metadata, peak_rss, summary
from terminal_acceptance import run as terminal_run


def library_run(binary, source, session):
    env = dict(os.environ, TEMP=str(session), TMP=str(session), TMPDIR=str(session))
    # Files avoid pipe deadlocks and retain child diagnostics even on failure.
    with tempfile.TemporaryFile(mode="w+") as stdout, tempfile.TemporaryFile(mode="w+") as stderr:
        process = subprocess.Popen([binary, str(source)], env=env, stdout=stdout, stderr=stderr)
        started = time.perf_counter()
        with Sampler(session, process._handle if os.name == "nt" else None) as sampler:
            peak = 0
            while process.returncode is None:
                if os.name == "nt":
                    process.poll()
                else:
                    pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                    if pid:
                        process.returncode = os.waitstatus_to_exitcode(status)
                        peak = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
                if time.perf_counter() - started > 120:
                    process.kill()
                    process.wait()
                    break
                time.sleep(.01)
        stdout.seek(0)
        stderr.seek(0)
        if os.name == "nt":
            sampler.rss = max(sampler.rss, peak_rss(process._handle))
        output, errors = stdout.read(), stderr.read()
        result = dict(kind="library", exit_status=process.returncode, status="failed", stderr=errors,
                      process_ms=(time.perf_counter() - started) * 1000, peak_rss_bytes=max(peak, sampler.rss),
                      sampled_temporary_disk_high_water_mark_bytes=sampler.disk, resource_sampling_errors=sampler.errors)
        if process.returncode == 0:
            try:
                result.update(json.loads(output), status="passed")
            except ValueError:
                result["error"] = "invalid child JSON"
        else:
            result["error"] = "child failed or exceeded 120-second watchdog"
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", help="built profile_open example")
    parser.add_argument("--terminal-binary", help="also measure actual PTY/ConPTY process")
    parser.add_argument("--rows", nargs="+", type=int, default=[100000, 1000000, 10000000])
    parser.add_argument("--shape", nargs="+", choices=SHAPES, default=list(SHAPES))
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--working-directory")
    parser.add_argument("--jsonl")
    parser.add_argument("--artifacts", default="artifacts/diagnostics")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    emit(dict(kind="environment", binary_sha256=checksum(args.binary), **metadata(args.working_directory or tempfile.gettempdir())), args.jsonl)
    for rows in args.rows:
        for shape in args.shape:
            results = []
            with tempfile.TemporaryDirectory(prefix="dataman-measure-", dir=args.working_directory) as root:
                root = Path(root)
                source = root / "fixture.sqlite"
                try:
                    fixture(source, rows, shape)
                except OSError as error:
                    emit(dict(kind="skip", rows=rows, shape=shape, reason=str(error)), args.jsonl)
                    continue
                for repetition in range(args.repetitions):
                    session = root / "session"
                    session.mkdir(exist_ok=True)
                    result = library_run(str(Path(args.binary).resolve()), source, session)
                    result.update(rows=rows, shape=shape, repetition=repetition, fixture_bytes=source.stat().st_size)
                    results.append(result)
                    emit(result, args.jsonl)
                    if args.terminal_binary:
                        for scenario in ("all", "count-cancel", "copy-cancel"):
                            result = terminal_run(str(Path(args.terminal_binary).resolve()), rows, shape, scenario,
                                                  args.working_directory, Path(args.artifacts) / f"{shape}-{rows}-{repetition}-{scenario}")
                            result["repetition"] = repetition
                            results.append(result)
                            emit(result, args.jsonl)
            for kind in ("library", "terminal"):
                emit(dict(kind="summary", process=kind, rows=rows, shape=shape,
                          metrics=summary([r for r in results if r["kind"] == kind])), args.jsonl)
    # Diagnostic failures and outliers are retained; functional CI uses the
    # acceptance command directly and enforces its exit status.


if __name__ == "__main__":
    main()
