"""Shared sequential-fixture and diagnostic helpers (standard library only)."""
import ctypes
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import sqlite3
import statistics
import subprocess
import threading
import time

SHAPES = ("narrow", "wide", "skewed")


def fixture(path, rows, shape):
    if rows < 100:
        raise ValueError("acceptance fixtures require at least 100 rows")
    # Budget for source, candidate, rollback journals, identity maps and headroom.
    required = rows * (1600 if shape == "wide" else 320) + 128 * 1024**2
    if shutil.disk_usage(path.parent).free < required:
        raise OSError(f"insufficient space: conservative budget {required} bytes")
    columns = ["id INTEGER PRIMARY KEY", "value TEXT"]
    expressions = ["x", "'value-'||x"]
    if shape == "skewed":
        expressions[1] = "CASE WHEN x%100<90 THEN 'repeated' WHEN x%100<99 THEN 'value-'||x ELSE NULL END"
    if shape == "wide":
        for i in range(2, 16):
            kind, expr = [("INTEGER", f"x*{i}"), ("REAL", f"x/{i}.0"),
                          ("TEXT", f"'mixed-{i}-'||x"), ("BLOB", "zeroblob(16)"),
                          ("TEXT", "NULL")][i % 5]
            columns.append(f"c{i} {kind}")
            expressions.append(expr)
    with sqlite3.connect(path) as db:
        db.execute(f"CREATE TABLE data({','.join(columns)})")
        db.execute(f"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<?) INSERT INTO data SELECT {','.join(expressions)} FROM n", (rows,))
    db.close()


def checksum(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inventory(root):
    result = []
    # Session cleanup races with sampling; os.walk tolerates vanished directories.
    for directory, _, files in os.walk(root):
        for name in files:
            path = Path(directory) / name
            try:
                result.append({"path": str(path.relative_to(root)), "bytes": path.stat().st_size})
            except FileNotFoundError:
                pass
    return result


def peak_rss(handle):
    class Counters(ctypes.Structure):
        _fields_ = [("cb", ctypes.c_ulong), ("faults", ctypes.c_ulong)] + [
            (name, ctypes.c_size_t) for name in ["peak", "working", "pool_peak", "pool", "nonpaged_peak", "nonpaged", "pagefile", "pagefile_peak"]]
    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    if ctypes.windll.psapi.GetProcessMemoryInfo(ctypes.c_void_p(int(handle)), ctypes.byref(counters), counters.cb):
        return counters.peak
    return 0


class Sampler:
    def __init__(self, root, handle=None):
        self.root, self.handle = root, handle
        self.disk = self.rss = 0
        self.errors = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self):
        while not self.stop.is_set():
            try:
                self.disk = max(self.disk, sum(p["bytes"] for p in inventory(self.root)))
                if os.name == "nt" and self.handle:
                    self.rss = max(self.rss, peak_rss(self.handle))
            except OSError as error:
                self.errors.append(str(error))
            self.stop.wait(0.01)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        if self.thread.ident is not None:
            self.thread.join()


def metadata(directory):
    def command(argv):
        try:
            return subprocess.check_output(argv, text=True, stderr=subprocess.STDOUT).strip()
        except (OSError, subprocess.CalledProcessError):
            return "unavailable"
    ram = None
    if os.name == "nt":
        class Memory(ctypes.Structure):
            _fields_ = [("length", ctypes.c_ulong), ("load", ctypes.c_ulong)] + [(n, ctypes.c_ulonglong) for n in ("total", "available", "page", "freepage", "virtual", "freevirtual", "extended")]
        memory = Memory()
        memory.length = ctypes.sizeof(memory)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(memory)):
            ram = memory.total
        fs = ctypes.create_unicode_buffer(256)
        ok = ctypes.windll.kernel32.GetVolumeInformationW(ctypes.c_wchar_p(Path(directory).resolve().anchor), None, 0, None, None, None, fs, len(fs))
        filesystem = fs.value if ok else "unavailable"
    else:
        if hasattr(os, "sysconf") and "SC_PHYS_PAGES" in os.sysconf_names:
            ram = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
        elif platform.system() == "Darwin":
            ram = command(["sysctl", "-n", "hw.memsize"])
        filesystem = command(["df", "-T" if platform.system() == "Linux" else "-P", str(directory)])
    git = ["git", "-c", "safe.directory=" + Path.cwd().as_posix()]
    return dict(os=platform.platform(), cpu=platform.processor(), ram_bytes=ram,
                filesystem=filesystem, working_directory=str(Path(directory).resolve()),
                rust=command(["rustc", "-Vv"]), cargo=command(["cargo", "-V"]),
                python=platform.python_version(), commit=command(git + ["rev-parse", "HEAD"]),
                source_status=command(git + ["status", "--short"]),
                source_diff_sha256=hashlib.sha256(command(git + ["diff", "HEAD"]).encode()).hexdigest(),
                runner=os.environ.get("RUNNER_OS"), ci_url=(f"https://github.com/{os.environ['GITHUB_REPOSITORY']}/actions/runs/{os.environ['GITHUB_RUN_ID']}" if "GITHUB_RUN_ID" in os.environ else None),
                cache="warm/cache-uncontrolled", sampling_interval_ms=10)


def emit(result, output=None):
    line = json.dumps(result)
    print(line, flush=True)
    if output:
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(line + "\n")


def summary(results):
    report = {}
    for key in sorted({k for r in results for k in r if k.endswith("_ms")}):
        values = sorted(r[key] for r in results if isinstance(r.get(key), (int, float)))
        if values:
            report[key] = dict(samples=len(values), median=statistics.median(values), minimum=min(values), maximum=max(values),
                               p95=values[math.ceil(len(values) * .95) - 1] if len(values) >= 20 else "unassessed (<20 samples)")
    return report
