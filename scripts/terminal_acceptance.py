"""Real-process PTY/ConPTY acceptance, including launch and terminal output.

python scripts/terminal_acceptance.py target/release/dataman.exe [100000]
Uses only Python's standard library. Fixtures live outside the measured process.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile
import time

from terminal_screen import Screen, verify_fragmentation
from diagnostics import SHAPES, Sampler, checksum, emit, fixture, inventory, metadata, peak_rss, summary


class WindowsTerminal:
    def __init__(self, argv, env):
        from ctypes import wintypes as w
        self.k = k = ctypes.WinDLL("kernel32", use_last_error=True)
        handle = w.HANDLE
        k.CreatePipe.argtypes = [ctypes.POINTER(handle), ctypes.POINTER(handle), ctypes.c_void_p, w.DWORD]
        k.CloseHandle.argtypes = [handle]
        k.PeekNamedPipe.argtypes = [handle, ctypes.c_void_p, w.DWORD, ctypes.c_void_p, ctypes.POINTER(w.DWORD), ctypes.c_void_p]
        k.ReadFile.argtypes = [handle, ctypes.c_void_p, w.DWORD, ctypes.POINTER(w.DWORD), ctypes.c_void_p]
        k.WriteFile.argtypes = k.ReadFile.argtypes
        k.WaitForSingleObject.argtypes = [handle, w.DWORD]
        k.TerminateProcess.argtypes = [handle, w.UINT]
        k.ClosePseudoConsole.argtypes = [handle]

        class Coord(ctypes.Structure):
            _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]

        class Startup(ctypes.Structure):
            _fields_ = [("cb", w.DWORD), ("reserved", w.LPWSTR), ("desktop", w.LPWSTR), ("title", w.LPWSTR)] + [
                (name, w.DWORD) for name in ["x", "y", "xs", "ys", "xc", "yc", "fill", "flags"]
            ] + [("show", w.WORD), ("reserved2size", w.WORD), ("reserved2", ctypes.c_void_p),
                 ("stdin", handle), ("stdout", handle), ("stderr", handle)]

        class StartupEx(ctypes.Structure):
            _fields_ = [("startup", Startup), ("attributes", ctypes.c_void_p)]

        class ProcessInfo(ctypes.Structure):
            _fields_ = [("process", handle), ("thread", handle), ("pid", w.DWORD), ("tid", w.DWORD)]

        k.CreatePseudoConsole.argtypes = [Coord, handle, handle, w.DWORD, ctypes.POINTER(handle)]
        k.InitializeProcThreadAttributeList.argtypes = [ctypes.c_void_p, w.DWORD, w.DWORD, ctypes.POINTER(ctypes.c_size_t)]
        k.UpdateProcThreadAttribute.argtypes = [ctypes.c_void_p, w.DWORD, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p]
        k.DeleteProcThreadAttributeList.argtypes = [ctypes.c_void_p]
        k.CreateProcessW.argtypes = [w.LPCWSTR, w.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, w.BOOL, w.DWORD, ctypes.c_void_p, w.LPCWSTR, ctypes.POINTER(StartupEx), ctypes.POINTER(ProcessInfo)]
        input_read, self.input, self.output, output_write = [handle() for _ in range(4)]
        assert k.CreatePipe(ctypes.byref(input_read), ctypes.byref(self.input), None, 0)
        assert k.CreatePipe(ctypes.byref(self.output), ctypes.byref(output_write), None, 0)
        self.console = handle()
        assert k.CreatePseudoConsole(Coord(160, 40), input_read, output_write, 0, ctypes.byref(self.console)) == 0
        k.CloseHandle(input_read)
        k.CloseHandle(output_write)
        size = ctypes.c_size_t()
        k.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
        attributes = ctypes.create_string_buffer(size.value)
        assert k.InitializeProcThreadAttributeList(attributes, 1, 0, ctypes.byref(size))
        assert k.UpdateProcThreadAttribute(attributes, 0, 0x20016, self.console, ctypes.sizeof(handle), None, None)
        startup = StartupEx()
        startup.startup.cb = ctypes.sizeof(startup)
        # Null standard handles prevent inheriting the launcher's redirected pipes.
        startup.startup.flags = 0x100
        startup.attributes = ctypes.cast(attributes, ctypes.c_void_p)
        info = ProcessInfo()
        block = ctypes.create_unicode_buffer("\0".join(f"{key}={value}" for key, value in sorted(env.items())) + "\0\0")
        command = ctypes.create_unicode_buffer(subprocess.list2cmdline(argv))
        assert k.CreateProcessW(None, command, None, None, False, 0x80400, block, None, ctypes.byref(startup), ctypes.byref(info)), ctypes.get_last_error()
        k.DeleteProcThreadAttributeList(attributes)
        k.CloseHandle(info.thread)
        self.process = info.process
        self.handle = info.process
        self.peak_rss = 0

    def read(self):
        available = ctypes.c_ulong()
        if not self.k.PeekNamedPipe(self.output, None, 0, None, ctypes.byref(available), None) or not available.value:
            time.sleep(0.002)
            return b""
        buffer = ctypes.create_string_buffer(available.value)
        read = ctypes.c_ulong()
        self.k.ReadFile(self.output, buffer, len(buffer), ctypes.byref(read), None)
        return buffer.raw[:read.value]

    def write(self, data):
        written = ctypes.c_ulong()
        assert self.k.WriteFile(self.input, data, len(data), ctypes.byref(written), None)

    def poll(self):
        code = ctypes.c_ulong()
        self.k.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        assert self.k.GetExitCodeProcess(self.process, ctypes.byref(code))
        return None if code.value == 259 else code.value

    def close(self):
        if self.poll() is None:
            self.k.TerminateProcess(self.process, 1)
            self.k.WaitForSingleObject(self.process, 2000)
        for handle in [self.input, self.output, self.process]:
            self.k.CloseHandle(handle)
        self.k.ClosePseudoConsole(self.console)


class PosixTerminal:
    def __init__(self, argv, env):
        import fcntl
        import pty
        import struct
        import termios
        self.fd, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.process = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=controlling_terminal)
        os.close(slave)
        self.handle = None
        self.peak_rss = 0

    def read(self):
        import select
        if select.select([self.fd], [], [], 0.01)[0]:
            try:
                return os.read(self.fd, 65536)
            except OSError:
                pass
        return b""

    def write(self, data):
        os.write(self.fd, data)

    def poll(self):
        if self.process.returncode is None:
            pid, status, usage = os.wait4(self.process.pid, os.WNOHANG)
            if pid:
                self.process.returncode = os.waitstatus_to_exitcode(status)
                self.peak_rss = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
        return self.process.returncode

    def close(self):
        if self.poll() is None:
            self.process.kill()
            pid, status, usage = os.wait4(self.process.pid, 0)
            self.process.returncode = os.waitstatus_to_exitcode(status)
            self.peak_rss = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
        os.close(self.fd)


def run(binary, rows, shape, scenario, directory, artifacts):
    with tempfile.TemporaryDirectory(prefix="dataman-terminal-", dir=directory) as root:
        root = Path(root)
        source = root / "source.sqlite"
        fixture(source, rows, shape)
        before = checksum(source)
        session = root / "session"
        session.mkdir()
        env = dict(os.environ, TERM="xterm-256color", TEMP=str(session), TMP=str(session), TMPDIR=str(session), RUST_LOG="off")
        transcript = bytearray()
        screen = Screen()
        timeline = []
        result = dict(kind="terminal", rows=rows, shape=shape, scenario=scenario, fixture_bytes=source.stat().st_size,
                      binary_sha256=checksum(binary))
        started = time.perf_counter()
        terminal = None
        sampler = Sampler(session)

        def event(label):
            timeline.append(dict(event=label, elapsed_ms=(time.perf_counter() - started) * 1000))
            print(label, file=sys.stderr, flush=True)

        def pump():
            data = terminal.read()
            transcript.extend(data)
            screen.feed(data)
            if screen.queries:
                terminal.write(b"\x1b[1;1R" * screen.queries)
                screen.queries = 0

        def wait_for(predicate, label, timeout=120):
            event("waiting: " + label)
            if terminal:
                debug = Path(artifacts)
                debug.mkdir(parents=True, exist_ok=True)
                (debug / "last-wait-screen.txt").write_text(screen.text, encoding="utf-8")
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                pump()
                if predicate(screen.text):
                    event("painted: " + label)
                    return
                if idle(screen.text) and ((label == "active F6 count" and f"total rows: {rows}," in screen.text)
                                         or (label == "active database copy" and "Edit Completed" in screen.text)):
                    raise AssertionError("Operation completed before active phase could be observed: " + label)
                code = terminal.poll()
                if code is not None:
                    raise AssertionError(f"Child exited {code} while waiting for {label}")
            raise AssertionError(f"Operation watchdog expired: {label}")

        def idle(text):
            return "Esc cancels" not in text

        def workspaces():
            return sorted(path for path in session.glob("dataman-*") if not path.name.startswith("dataman-identities-"))

        def await_value(value):
            def committed(text):
                if not idle(text) or not re.search(r"(?m)^[│|]\s*1\s+(?:1\s+)?" + re.escape(value) + r"(?:\s|│|$)", text):
                    return False
                for workspace in workspaces():
                    path = workspace / "session.sqlite"
                    if not path.exists():
                        continue
                    # Read-only inspection must never create a candidate file.
                    connection = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=.01)
                    try:
                        if connection.execute("SELECT value FROM data WHERE id=1").fetchone() == (value,):
                            return True
                    except sqlite3.Error:
                        pass
                    finally:
                        connection.close()
                return False
            wait_for(committed, "committed and painted " + value)

        try:
            terminal = (WindowsTerminal if os.name == "nt" else PosixTerminal)([binary, str(source)], env)
            sampler.handle = terminal.handle
            sampler.__enter__()
            first_value = "repeated" if shape == "skewed" else "value-1"
            wait_for(lambda t: first_value in t and "unknown" in t and idle(t), "first usable page")
            result["launch_to_painted_page_ms"] = (time.perf_counter() - started) * 1000
            assert not workspaces(), "Opening copied the database"
            if scenario == "count-cancel":
                terminal.write(b"\x1b[17~")
                wait_for(lambda t: "Esc cancels" in t, "active F6 count")
                cancelled = time.perf_counter()
                terminal.write(b"\x1b")
                wait_for(lambda t: "Operation cancelled" in t and idle(t), "active count cancellation", timeout=10)
                result["count_cancellation_ack_ms"] = (time.perf_counter() - cancelled) * 1000
                assert "unknown" in screen.text, "Cancelled count published an exact total"
                assert not workspaces(), "Cancelled count promoted"
            counted = time.perf_counter()
            terminal.write(b"\x1b[17~")
            wait_for(lambda t: f"total rows: {rows}," in t and idle(t), "exact F6 total")
            result["count_ms"] = (time.perf_counter() - counted) * 1000
            assert not workspaces(), "Counting promoted the source"
            navigated = time.perf_counter()
            terminal.write(b"\x1b[B")
            wait_for(lambda t: "row: 1," in t and idle(t), "cached cursor down")
            result["cached_navigation_paint_ms"] = (time.perf_counter() - navigated) * 1000
            terminal.write(b"\x1b[A")
            wait_for(lambda t: "row: 0," in t and idle(t), "cursor up")
            if scenario in ("all", "stale-edit"):
                with sqlite3.connect(source) as writer:
                    writer.execute("UPDATE data SET value='external-change' WHERE id=1")
                writer.close()
                before = checksum(source)  # The authorized external writer changed it.
                terminal.write(b"\x1b[Ceobsolete\r")
                wait_for(lambda t: "review and retry" in t and idle(t), "stale edit rejected")
                assert not workspaces(), "Stale edit published a workspace"
                with sqlite3.connect(source) as reader:
                    assert reader.execute("SELECT value FROM data WHERE id=1").fetchone() == ("external-change",)
                reader.close()
                # F6 runs on the worker and refreshes the displayed identities.
                terminal.write(b"\x1b[17~")
                wait_for(lambda t: "external-change" in t and "last command: Count Rows" in t and idle(t), "refreshed source")
            else:
                terminal.write(b"\x1b[C")
                # Wait for a complete navigation response before opening a prompt.
                pump()
            if scenario == "copy-cancel":
                terminal.write(b"eedited\r")
                wait_for(lambda t: "Copying database" in t and "Esc cancels" in t, "active database copy")
                cancelled = time.perf_counter()
                terminal.write(b"\x1b")
                wait_for(lambda t: "Operation cancelled" in t and idle(t), "active copy cancellation", timeout=10)
                result["copy_cancellation_ack_ms"] = (time.perf_counter() - cancelled) * 1000
                assert not workspaces(), "Cancelled copy published a candidate"
                assert checksum(source) == before
            if scenario in ("all", "mutation", "copy-cancel"):
                promoted = time.perf_counter()
                terminal.write(b"eedited\r")
                await_value("edited")
                result["first_edit_ms"] = (time.perf_counter() - promoted) * 1000
                first = workspaces()
                assert len(first) == 1, first
                terminal.write(b"esecond\r")
                await_value("second")
                assert workspaces() == first, "Second edit created another workspace"
                terminal.write(b"\x1a")
                wait_for(lambda t: "Undo" in t and idle(t), "Undo")
                await_value("edited")
                terminal.write(b"\x19")
                wait_for(lambda t: "Redo" in t and idle(t), "Redo")
                await_value("second")
            if scenario in ("all", "cancel"):
                terminal.write(b"qWITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n;\r")
                wait_for(lambda t: "Esc cancels" in t, "active SQL")
                cancelled = time.perf_counter()
                terminal.write(b"\x1b")
                wait_for(lambda t: "Operation cancelled" in t and idle(t), "cancellation", timeout=10)
                result["cancellation_ack_ms"] = (time.perf_counter() - cancelled) * 1000
            verify_fragmentation(bytes(transcript))
            capture_dir = Path(artifacts)
            capture_dir.mkdir(parents=True, exist_ok=True)
            (capture_dir / "terminal-capture.vt").write_bytes(transcript)
            terminal.write(b"\x03")
            deadline = time.monotonic() + 120
            while terminal.poll() is None and time.monotonic() < deadline:
                pump()
            result["exit_status"] = terminal.poll()
            assert result["exit_status"] == 0, f"Ungraceful exit: {result['exit_status']}"
            assert checksum(source) == before, "Source changed"
            assert not list(session.iterdir()), "Session storage was not cleaned up"
            result["status"] = "passed"
        except Exception as error:
            result.update(status="failed", error=str(error), exit_status=terminal.poll() if terminal else None)
            artifacts = Path(artifacts)
            artifacts.mkdir(parents=True, exist_ok=True)
            # Fixtures are generated test data; artifacts contain terminal output and
            # file names/sizes only. Never copy any SQLite database into artifacts.
            (artifacts / "transcript.bin").write_bytes(transcript)
            (artifacts / "screen.txt").write_text(screen.text, encoding="utf-8")
            (artifacts / "timeline.json").write_text(json.dumps(timeline, indent=2), encoding="utf-8")
            (artifacts / "session-files.json").write_text(json.dumps(inventory(session), indent=2), encoding="utf-8")
            (artifacts / "failure.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
        finally:
            sampler.__exit__()
            if terminal:
                if os.name == "nt":
                    sampler.rss = max(sampler.rss, peak_rss(terminal.handle))
                terminal.close()  # Force only after recording a failed watchdog/exit.
            result.update(peak_rss_bytes=max(sampler.rss, terminal.peak_rss if terminal else 0),
                          sampled_temporary_disk_high_water_mark_bytes=sampler.disk,
                          resource_sampling_errors=sampler.errors)
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("rows", type=int, nargs="?", default=100_000)
    parser.add_argument("--shape", choices=SHAPES, default="narrow")
    parser.add_argument("--scenario", choices=("all", "browse", "mutation", "cancel", "stale-edit", "count-cancel", "copy-cancel"), default="all")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--working-directory")
    parser.add_argument("--jsonl")
    parser.add_argument("--artifacts", default="artifacts/terminal")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    emit(dict(kind="environment", **metadata(args.working_directory or tempfile.gettempdir())), args.jsonl)
    results = []
    for i in range(args.repetitions):
        try:
            result = run(str(Path(args.binary).resolve()), args.rows, args.shape, args.scenario,
                         args.working_directory, Path(args.artifacts) / f"{args.shape}-{args.rows}-{i}")
        except Exception as error:
            result = dict(kind="terminal", rows=args.rows, shape=args.shape, status="failed", error=str(error), exit_status=None)
        results.append(result)
        emit(result, args.jsonl)
    emit(dict(kind="summary", rows=args.rows, shape=args.shape, metrics=summary(results)), args.jsonl)
    return int(any(r["status"] != "passed" for r in results))


if __name__ == "__main__":
    sys.exit(main())
