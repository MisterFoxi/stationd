#!/usr/bin/env python3
"""Real A/B switching: same HTTP listener, decoded frequency and isolated control plane."""
import array
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml")]
BASE = "http://" + os.environ.get("ICECAST_BIND", "192.168.1.118") + ":" + os.environ.get("ICECAST_PORT", "18000")


def compose(*args, check=True, **kwargs):
    return subprocess.run([*DC, *args], cwd=ROOT, check=check, **kwargs)


def ctl(*args, success=True):
    result = compose("exec", "-T", "admin", "stationctl", "icecast", *args,
                     check=False, capture_output=True, text=True, timeout=20)
    assert (result.returncode == 0) == success, result.stderr
    return json.loads(result.stdout) if success else result.stderr


def wait(predicate, label, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.3)
    raise AssertionError("Timeout: " + label)


def route_ready(selected=None):
    route = ctl("route")
    return route if route["connected"] and (selected is None or route["input"] == selected) else None


def tone(stream, expected, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        encoded = stream.read(8192)
        assert len(encoded) == 8192, "stable listener disconnected"
        decoded = compose(
            "exec", "-T", "live-a", "ffmpeg", "-hide_banner", "-loglevel", "error",
            "-f", "mp3", "-i", "pipe:0", "-f", "s16le", "-ac", "1", "-ar", "44100", "pipe:1",
            input=encoded, capture_output=True, check=False, timeout=10,
        )
        assert decoded.returncode == 0, decoded.stderr.decode(errors="replace")
        samples = array.array("h", decoded.stdout)
        assert len(samples) >= 10000, "insufficient decoded audio"
        # Discard chunk edges; the fixtures are pure sinusoids.
        samples = samples[2000:-2000]
        crossings = sum((a < 0) != (b < 0) for a, b in zip(samples, samples[1:]))
        frequency = crossings * 44100 / (2 * len(samples))
        if abs(frequency - expected) < expected * 0.02:
            return round(frequency, 1)
    raise AssertionError(f"did not hear {expected} Hz after switch")


def main():
    wait(lambda: route_ready(), "router ready")
    ctl("switch", "a")
    first_container = subprocess.check_output([*DC, "ps", "-q", "icecast"], text=True).strip()
    started = subprocess.check_output(
        ["docker", "inspect", "-f", "{{.State.StartedAt}}", first_container], text=True)
    try:
        with urllib.request.urlopen(BASE + "/radio.mp3", timeout=10) as stream:
            assert stream.status == 200
            assert "audio/mpeg" in stream.headers["Content-Type"]
            a = tone(stream, 440)
            listeners = wait(lambda: ctl("listeners", "/radio.mp3"), "stable listener listed")
            listener_id = listeners[0]["id"]
            assert ctl("switch", "b")["input"] == "b"
            b = tone(stream, 880)
            assert any(l["id"] == listener_id for l in ctl("listeners", "/radio.mp3"))
            ctl("switch", "a")
            tone(stream, 440)
            invalid = ctl("switch", "unknown-input", success=False)
            assert "InvalidArgument" in invalid
            compose("stop", "live-b", stdout=subprocess.DEVNULL)
            wait(lambda: not next(m for m in ctl("status")["mounts"]
                                  if m["mount"] == "/test-b.mp3")["connected"], "B disconnected")
            refused = ctl("switch", "b", success=False)
            assert "FailedPrecondition" in refused
            assert route_ready("a")
            tone(stream, 440)
            compose("--profile", "live", "up", "-d", "live-b", stdout=subprocess.DEVNULL)
            wait(lambda: next(m for m in ctl("status")["mounts"]
                              if m["mount"] == "/test-b.mp3")["connected"], "B restored")
            ctl("switch", "b")
            tone(stream, 880)
            compose("stop", "admin", stdout=subprocess.DEVNULL)
            tone(stream, 880)
            compose("up", "-d", "--wait", "admin", stdout=subprocess.DEVNULL)
            assert route_ready("b")
            assert any(l["id"] == listener_id for l in ctl("listeners", "/radio.mp3"))
            print(f"PASS: same listener {listener_id}: A={a} Hz -> B={b} Hz -> A; invalid/unavailable target refused; admin outage preserved B.")

        # A stopped source must not prevent a manual recovery to live B.
        ctl("switch", "a")
        compose("stop", "live-a", stdout=subprocess.DEVNULL)
        wait(lambda: not ctl("route")["connected"], "A failure detected")
        assert ctl("switch", "b")["connected"]
        compose("--profile", "live", "up", "-d", "live-a", stdout=subprocess.DEVNULL)
        wait(lambda: next(m for m in ctl("status")["mounts"]
                          if m["mount"] == "/test-a.mp3")["connected"], "A restored")
        print("PASS: manual switch to B also recovers an unavailable current source A.")

        # Restart only the data plane: selection must survive in its runtime volume.
        compose("restart", "router", stdout=subprocess.DEVNULL)
        wait(lambda: route_ready("b"), "persisted B after router restart")
        with urllib.request.urlopen(BASE + "/radio.mp3", timeout=10) as stream:
            tone(stream, 880)
        after = subprocess.check_output(
            ["docker", "inspect", "-f", "{{.State.StartedAt}}", first_container], text=True)
        assert started == after, "Icecast was restarted during the switch test"
        print("PASS: new listener hears B after router restart; Icecast not restarted.")
    finally:
        compose("--profile", "live", "up", "-d", "live-a", "live-b", stdout=subprocess.DEVNULL)
        compose("up", "-d", "--wait", "admin", stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    main()

