#!/usr/bin/env python3
"""Live persistent history and administrative restart, without restarting Icecast."""
import json
from pathlib import Path
import subprocess
import threading
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml")]
UA = "stationd-v1-history-" + uuid.uuid4().hex[:10]


def ctl(*args):
    return json.loads(subprocess.check_output([*DC, "exec", "-T", "admin", "stationctl", "icecast", *args], text=True))


def history():
    return ctl("history", "--since", "1h", "--mount", "/radio.mp3")


def wait_for(fn, seconds=50):
    deadline = time.monotonic() + seconds
    while True:
        result = fn()
        if result:
            return result
        assert time.monotonic() < deadline, "history deadline exceeded"
        time.sleep(.5)


first_container = subprocess.check_output([*DC, "ps", "-q", "icecastd"], text=True).strip()
started = subprocess.check_output(["docker", "inspect", "-f", "{{.State.StartedAt}}", first_container], text=True)
request = urllib.request.Request("https://v1stream.homestone.rp-radio.live/", headers={"User-Agent": UA})
with urllib.request.urlopen(request, timeout=10) as stream:
    stop = threading.Event()
    observed = [0]
    errors = []

    def read_audio():
        try:
            while not stop.is_set():
                data = stream.read(4096)
                if not data:
                    raise RuntimeError("audio stream closed")
                observed[0] += len(data)
        except Exception as error:
            errors.append(str(error))

    thread = threading.Thread(target=read_audio, daemon=True)
    thread.start()
    try:
        listener_id = wait_for(lambda: next((l["id"] for l in ctl("listeners", "/radio.mp3") if l["user_agent"] == UA), None), 8)
        first = wait_for(lambda: next((row for row in history()["statistics"]["rows"] if row[6] >= 1), None))
        before = history()["coverage"]["rows"][0][1]
        received = observed[0]
        subprocess.run([*DC, "up", "-d", "--no-deps", "--force-recreate", "--wait", "admin"], check=True, timeout=150)
        assert any(l["id"] == listener_id for l in ctl("listeners", "/radio.mp3"))
        assert history()["coverage"]["rows"][0][1] >= before
        wait_for(lambda: history()["coverage"]["rows"][0][1] > before)
        assert observed[0] > received and not errors, errors
        assert ctl("history-status")["plugins"][0]["problem"] == ""
        assert started == subprocess.check_output(["docker", "inspect", "-f", "{{.State.StartedAt}}", first_container], text=True)
        print("PASS: real Traefik listener recorded; history preserved across admin recreation; sampling resumed; same listener and Icecast survived; audio bytes", observed[0])
    finally:
        stop.set()
        thread.join(timeout=2)
