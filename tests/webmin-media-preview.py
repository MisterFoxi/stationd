#!/usr/bin/env python3
"""Read-only PTY smoke test against a station with an indexed MP3.

Run inside the dev container:
    python3 tests/webmin-media-preview.py http://devstationd.lan:50051
"""
import argparse
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import termios
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("address", help="gRPC endpoint of a running station with indexed MP3 media")
parser.add_argument("--binary", default="target/active/stationd-tui")
parser.add_argument("--plain-terminal", action="store_true", help="Check visible shortcut and explicit missing-player feedback without Webmin environment")
args = parser.parse_args()
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
env = {"TERM": "xterm-256color", "LANG": "C.UTF-8"}
if not args.plain_terminal:
    env["STATIOND_WEBMIN_MEDIA_PREVIEW"] = "1"
process = subprocess.Popen(
    [args.binary, "--addr", args.address, "--lang", "fr"],
    stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True,
)
os.close(slave)


def read(seconds):
    output = b""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], max(0, deadline - time.monotonic()))[0]:
            data = os.read(master, 65536)
            output += data
            if b"\x1b[6n" in data:
                os.write(master, b"\x1b[1;1R")
    return output


try:
    read(3)
    os.write(master, b"5")  # Media screen, initially in tree view.
    read(2)
    os.write(master, b"v")  # Flat view includes files nested under genre folders.
    read(2)
    os.write(master, b"e")  # Open the first media's tag form; never submit it.
    form = read(3)
    assert b"Ctrl+P" in form, "Preview shortcut absent: check endpoint, indexed media and TUI binary"
    os.write(master, b"\x10")
    preview = read(1)
    if args.plain_terminal:
        # Differential rendering omits unchanged spaces already on the screen.
        text = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", preview)
        assert b"aucunlecteurWebmin" in text.replace(b" ", b""), "Missing-player feedback absent"
        assert b"stationd-preview;" not in preview, "Plain terminal emitted audio controls"
    else:
        assert re.search(rb"\x1b\]778;stationd-preview;[0-9a-f]+\x07", preview), "Preview OSC missing after Ctrl+P"
    os.write(master, b"\x1b")
    closed = read(1)
    if not args.plain_terminal:
        assert b"\x1b]778;stationd-preview;stop\x07" in closed, "Closing form did not stop preview"
    print("Real PTY: Ctrl+P visible; " + ("explicit missing-player feedback" if args.plain_terminal else "preview OSC and close/stop") + " passed (read-only).")
finally:
    process.terminate()
    process.wait(timeout=5)
    os.close(master)
