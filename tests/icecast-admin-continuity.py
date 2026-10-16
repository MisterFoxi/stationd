#!/usr/bin/env python3
"""Run on stationdev1: audio must continue while the admin daemon is stopped."""
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml")]
BASE = "http://" + os.environ.get("ICECAST_BIND", "192.168.1.118") + ":" + os.environ.get("ICECAST_PORT", "18000")


def compose(*args, **kwargs):
    return subprocess.run([*DC, *args], cwd=ROOT, check=True, **kwargs)


def main():
    running_router = subprocess.check_output([*DC, "ps", "--status", "running", "-q", "router"], cwd=ROOT, text=True).strip()
    assert not running_router, "Stop switching fixtures first: make icecast-switch-down"
    name = "stationd-icecast-continuity-" + uuid.uuid4().hex[:12]
    created = False
    try:
        compose("run", "-d", "--no-deps", "--name", name, "--entrypoint", "python3",
                "smoke", "/test/icecast-smoke.py", "--continuity-source",
                stdout=subprocess.DEVNULL)
        created = True
        deadline = time.monotonic() + 15
        while True:
            try:
                stream = urllib.request.urlopen(BASE + "/test-a.mp3", timeout=5)
                break
            except (urllib.error.URLError, TimeoutError):
                if time.monotonic() >= deadline:
                    raise AssertionError("test source did not connect")
                time.sleep(0.2)
        with stream:
            assert len(stream.read(8192)) == 8192
            container = subprocess.check_output([*DC, "ps", "-q", "icecast"], cwd=ROOT, text=True).strip()
            started = subprocess.check_output(
                ["docker", "inspect", "-f", "{{.State.StartedAt}}", container], text=True)
            compose("stop", "admin")
            deadline = time.monotonic() + 4
            received = 0
            while time.monotonic() < deadline:
                chunk = stream.read(4096)
                assert chunk, "audio disconnected while admin stopped"
                received += len(chunk)
            assert received >= 16384, "not enough live audio during admin outage"
            compose("up", "-d", "--wait", "admin")
            assert len(stream.read(8192)) == 8192, "audio disconnected on admin restart"
            after = subprocess.check_output(
                ["docker", "inspect", "-f", "{{.State.StartedAt}}", container], text=True)
            assert started == after, "Icecast was restarted with the admin daemon"
            print(f"PASS: {received} audio bytes during admin outage; same listener survives admin restart; Icecast not restarted.")
    finally:
        # Always restore administration and remove only our uniquely named source.
        compose("up", "-d", "--wait", "admin")
        if created:
            subprocess.run(["docker", "rm", "-f", name], check=True, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    main()

