#!/usr/bin/env python3
"""Real Linux PTY check: python3 tests/webmin-tui-diagnostics.py [TUI binary]."""
import errno
import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

MARKER = re.compile(rb"\x1b\]777;stationd;(\d+)\x07")

def launch(enabled):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    env = {**os.environ, "TERM": "xterm-256color", "LANG": "C.UTF-8"}
    env.pop("STATIOND_WEBMIN_DIAGNOSTICS", None)
    if enabled:
        env["STATIOND_WEBMIN_DIAGNOSTICS"] = "1"
    process = subprocess.Popen([sys.argv[1] if len(sys.argv) > 1 else "target/debug/stationd-tui", "--addr", "http://127.0.0.1:1"], stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
    os.close(slave)
    return process, master

def read(master, seconds):
    output = b""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], max(0, deadline-time.monotonic()))[0]:
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            output += data
            # Answer crossterm cursor-position queries during initialization.
            if b"\x1b[6n" in data:
                os.write(master, b"\x1b[1;1R")
    return output

for enabled in (False, True):
    process, master = launch(enabled)
    try:
        initial = read(master, 3)
        assert process.poll() is None, initial.decode(errors="replace")
        if not enabled:
            assert not MARKER.search(initial), "Telemetry must be opt-in"
            continue
        assert MARKER.search(initial), "Event-loop heartbeat absent"
        baseline = int(MARKER.findall(initial)[-1])
        # Unassigned key still passes through the handler and is counted.
        os.write(master, b"z")
        after_key = read(master, 1.5)
        assert any(int(value) > baseline for value in MARKER.findall(after_key)), "TUI key reception absent"
        os.kill(process.pid, signal.SIGSTOP)
        read(master, 0.2)  # Drain already emitted output.
        assert not MARKER.search(read(master, 1.5)), "Stopped loop still emitted heartbeat"
        os.kill(process.pid, signal.SIGCONT)
        assert MARKER.search(read(master, 2)), "Heartbeat did not recover"
    finally:
        os.kill(process.pid, signal.SIGCONT)
        process.terminate()
        process.wait(timeout=5)
        os.close(master)
print("TUI diagnostics: opt-in, real PTY key reception, stopped-loop silence and recovery passed.")
