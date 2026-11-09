"""Real-process PTY/ConPTY acceptance, including launch and terminal output.

python scripts/terminal_acceptance.py target/release/dataman.exe [100000]
Uses only Python's standard library. Fixtures live outside the measured process.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile
import time


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

    def close(self):
        if self.k.WaitForSingleObject(self.process, 2000) != 0:
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

    def close(self):
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        os.close(self.fd)


binary = str(Path(sys.argv[1]).resolve())
rows = int(sys.argv[2]) if len(sys.argv) > 2 else 100_000
with tempfile.TemporaryDirectory(prefix="dataman-terminal-") as root:
    root = Path(root)
    source = root / "source.sqlite"
    with sqlite3.connect(source) as db:
        db.executescript(f"""CREATE TABLE data(id INTEGER PRIMARY KEY,value TEXT);
            WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<{rows})
            INSERT INTO data SELECT x,'value-'||x FROM n;""")
    db.close()
    before = hashlib.sha256(source.read_bytes()).digest()
    session = root / "session"
    session.mkdir()
    env = dict(os.environ, TERM="xterm-256color", TEMP=str(session), TMP=str(session), TMPDIR=str(session))
    started = time.perf_counter()
    terminal = (WindowsTerminal if os.name == "nt" else PosixTerminal)([binary, str(source)], env)
    transcript = bytearray()

    def wait_for(text, timeout=15):
        output = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            data = terminal.read()
            transcript.extend(data)
            output.extend(data)
            if b"\x1b[6n" in data:
                terminal.write(b"\x1b[1;1R")
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
            if re.sub(rb"\s+", b"", text.encode()) in re.sub(rb"\s+", b"", plain):
                return
        raise AssertionError(f"Missing terminal output {text!r}: {output[-3000:]!r}")

    def workspaces():
        return sorted(path for path in session.glob("dataman-*") if not path.name.startswith("dataman-identities-"))

    def await_value(value):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            for workspace in workspaces():
                path = workspace / "session.sqlite"
                if path.exists():
                    connection = sqlite3.connect(path, timeout=0.01)
                    try:
                        if connection.execute("SELECT value FROM data WHERE id=1").fetchone() == (value,):
                            return
                    except sqlite3.Error:
                        pass  # The candidate is still being copied or initialized.
                    finally:
                        connection.close()
            transcript.extend(terminal.read())
        raise AssertionError(f"Workspace value did not become {value!r}")

    try:
        wait_for("value-1")
        assert b"unknown" in transcript
        opening_ms = (time.perf_counter() - started) * 1000
        assert not workspaces(), "Opening copied the database"
        terminal.write(b"\x1b[17~")  # F6
        wait_for(str(rows))
        assert not workspaces(), "Counting promoted the source"
        navigated = time.perf_counter()
        terminal.write(b"\x1b[B")  # down within the cached page
        wait_for("value-2")
        navigation_ms = (time.perf_counter() - navigated) * 1000
        terminal.write(b"\x1b[A")
        wait_for("value-1")
        terminal.write(b"\x1b[Ceedited\r")  # right, edit, submit
        wait_for("edited")
        # Input painting also contains edited; confirm the mutation committed.
        await_value("edited")
        first = workspaces()
        assert len(first) == 1, first
        time.sleep(0.1)
        terminal.write(b"esecond\r")
        wait_for("second")
        await_value("second")
        time.sleep(0.1)
        assert workspaces() == first, "Second edit created another workspace"
        terminal.write(b"\x1a")  # undo
        wait_for("Undo")
        await_value("edited")
        terminal.write(b"\x19")  # redo
        wait_for("Redo")
        await_value("second")
        terminal.write(b"qWITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n;\r")
        time.sleep(0.2)
        cancelled = time.perf_counter()
        terminal.write(b"\x1b")
        wait_for("Operation cancelled")
        cancellation_ms = (time.perf_counter() - cancelled) * 1000
        terminal.write(b"\x03")
    finally:
        terminal.close()
    assert hashlib.sha256(source.read_bytes()).digest() == before, "Source changed"
    assert not list(session.iterdir()), "Session storage was not cleaned up"
    print(json.dumps(dict(kind="terminal", rows=rows, launch_to_painted_page_ms=round(opening_ms, 3),
                          cached_navigation_paint_ms=round(navigation_ms, 3),
                          cancellation_ack_ms=round(cancellation_ms, 3))))
