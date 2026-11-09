"""Measure a release profile_open binary; fixtures and build are outside timing.

Usage: python scripts/measure_open.py target/release/examples/profile_open.exe
Outputs single-run warm-cache observations, not percentile acceptance results.
"""
import ctypes
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time


def peak_rss(process):
    if os.name != "nt":
        return 0  # POSIX wait4 rusage is collected after process completion.

    class Counters(ctypes.Structure):
        _fields_ = [("cb", ctypes.c_ulong), ("faults", ctypes.c_ulong)] + [
            (name, ctypes.c_size_t) for name in [
                "peak", "working", "pool_peak", "pool", "nonpaged_peak",
                "nonpaged", "pagefile", "pagefile_peak",
            ]
        ]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    if ctypes.windll.psapi.GetProcessMemoryInfo(
        ctypes.c_void_p(int(process._handle)), ctypes.byref(counters), counters.cb
    ):
        return counters.peak
    return 0


binary = str(Path(sys.argv[1]).resolve())
for count in [100_000, 1_000_000, 10_000_000]:
    with tempfile.TemporaryDirectory(prefix="dataman-measure-") as root:
        root = Path(root)
        source = root / "fixture.sqlite"
        with sqlite3.connect(source) as db:
            db.executescript(f"""
                CREATE TABLE data(id INTEGER PRIMARY KEY,value TEXT,amount INTEGER);
                WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<{count})
                INSERT INTO data SELECT id,'value-'||(id%1000),id*2 FROM n;
            """)
        db.close()
        session = root / "session"
        session.mkdir()
        env = dict(os.environ, TEMP=str(session), TMP=str(session), TMPDIR=str(session))
        started = time.perf_counter()
        process = subprocess.Popen([binary, str(source)], env=env, stdout=subprocess.PIPE, text=True)
        peak = 0
        if os.name == "nt":
            while process.poll() is None:
                peak = max(peak, peak_rss(process))
                time.sleep(0.001)
        else:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
            peak = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
        stdout, _ = process.communicate()
        if process.returncode:
            raise RuntimeError(f"profile process failed: {process.returncode}")
        result = json.loads(stdout)
        result.update(kind="library", process_ms=round((time.perf_counter()-started)*1000, 3),
                      peak_rss_bytes=peak, fixture_bytes=source.stat().st_size)
        print(json.dumps(result), flush=True)
