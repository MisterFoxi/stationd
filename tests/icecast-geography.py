#!/usr/bin/env python3
"""Isolated Icecast: forwarded IP trust and native client IP and central GeoLite.
Downloads only MaxMind's small public test fixture into ignored .local/.
Never publishes ports or touches the normal bench project.
"""
import json
import shutil
from pathlib import Path
import subprocess
import time
import urllib.request
import uuid
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
PROJECT = "stationd-geo-test-" + uuid.uuid4().hex[:10]
WORK = ROOT / ".local/icecast/geotest" / PROJECT
WORK.mkdir(parents=True, mode=0o755)
WORK.chmod(0o755)
GEO = WORK / "geoip"
GEO.mkdir()
CONFIG = WORK / "config"
CONFIG.mkdir()
FIXTURE = "https://raw.githubusercontent.com/maxmind/MaxMind-DB/main/test-data/GeoLite2-City-Test.mmdb"
with urllib.request.urlopen(FIXTURE, timeout=20) as response:
    data = response.read(2 * 1024 * 1024 + 1)
assert 1024 < len(data) < 2 * 1024 * 1024, "invalid fixture size"
(GEO / "GeoLite2-City.mmdb").write_bytes(data)
override = WORK / "compose.json"
# Compose !reset removes inherited host publication rather than merging an empty list.
override.write_text("services:\n  icecastd:\n    ports: !reset []\n    volumes:\n      - " + str(CONFIG) + ":/etc/stationd:ro\n      - " + str(GEO) + ":/data/geoip:ro\n  admin:\n    ports: !reset []\n    volumes:\n      - " + str(CONFIG) + ":/etc/stationd:ro\n      - " + str(GEO) + ":/data/geoip:ro\n  smoke:\n    image: stationd-v1-icecast-live:dev\n    pull_policy: never\n")
DC = ["docker", "compose", "-p", PROJECT, "-f", str(ROOT / "docker/icecast/compose.yaml"), "-f", str(ROOT / "docker/icecast/compose.standalone-test.yaml"), "-f", str(override), "--profile", "test", "--profile", "live"]

def run(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout

def dc(*args):
    return run(*DC, *args)

def ctl(*args):
    return json.loads(dc("exec", "-T", "admin", "stationctl", "icecast", *args))

clients = []
try:
    for label in ("trusted", "untrusted"):
        name = PROJECT + "-" + label
        dc("run", "--rm", "--no-deps", "-d", "--name", name, "--entrypoint", "python3", "smoke", "-c", "import time; time.sleep(180)")
        clients.append(name)
    info = json.loads(run("docker", "inspect", clients[0]))[0]
    proxy_ip = next(iter(info["NetworkSettings"]["Networks"].values()))["IPAddress"]
    text = (ROOT / "config/icecast/icecast.toml").read_text()
    import re
    text = re.sub(r'trusted_proxies = \[[^\n]*', 'trusted_proxies = ["' + proxy_ip + '"]', text)
    text = text.replace('audience = false', 'audience = true', 1).replace('audience = true\nhidden = false', 'audience = false\nhidden = false')
    (CONFIG / "icecast.toml").write_text(text)
    (CONFIG / "admin.toml").write_text((ROOT / "config/icecast/admin.toml").read_text().replace("sample_interval_seconds = 30", "sample_interval_seconds = 5"))
    dc("up", "-d", "--no-build", "--wait", "icecastd", "admin", "live-a")
    deadline = time.monotonic() + 20
    while not any(m["connected"] and m["mount"] == "/test-a.mp3" for m in ctl("status")["mounts"]):
        assert time.monotonic() < deadline, "source not ready"
        time.sleep(.3)
    listen = "import urllib.request; r=urllib.request.urlopen(urllib.request.Request('http://icecastd:8000/test-a.mp3',headers={'X-Forwarded-For':'81.2.69.160'}),timeout=120); exec('while r.read(4096): pass')"
    for name in clients:
        run("docker", "exec", "-d", name, "python3", "-c", listen)
    deadline = time.monotonic() + 10
    while True:
        listeners = ctl("listeners", "/test-a.mp3")
        if len(listeners) == 2:
            break
        assert time.monotonic() < deadline, "listeners not ready"
        time.sleep(.3)
    found = [l for l in listeners if l["lookup_status"] == "found"]
    private = [l for l in listeners if l["lookup_status"] == "private"]
    assert len(found) == len(private) == 1, listeners
    assert found[0]["ip"] == "81.2.69.160" and found[0]["country"] == "GB", found
    assert found[0]["city"] == "London", found
    snapshot = ctl("geography")
    assert snapshot["complete"] and snapshot["geoip_status"] == "ready", snapshot
    assert sum(b["listeners"] for b in snapshot["buckets"]) == 2, snapshot
    assert any(b["country"] == "GB" and b["city"] == "London" for b in snapshot["buckets"]), snapshot
    deadline = time.monotonic() + 15
    while True:
        history = ctl("history", "--since", "1h", "--mount", "/test-a.mp3")
        if any(row[2] == "GB" and row[4] == "London" for row in history["statistics"]["rows"]):
            break
        assert time.monotonic() < deadline, history
        time.sleep(.5)
    assert ctl("history-status")["plugins"][0]["problem"] == ""
    native_code = "import urllib.request,base64; from pathlib import Path; p=Path('/run/secrets/admin-password').read_text().strip(); q=urllib.request.Request('http://icecastd:8000/admin/listclients?mount=/test-a.mp3'); q.add_header('Authorization','Basic '+base64.b64encode(('admin:'+p).encode()).decode()); print(urllib.request.urlopen(q).read().decode())"
    native = run("docker", "exec", clients[0], "python3", "-c", native_code)
    root = ET.fromstring(native)
    assert "81.2.69.160" in [e.text for e in root.iter() if e.tag.split('}')[-1] == "ip"], native
    print("PASS: trusted forwarded IP = GB/London; untrusted header ignored; native IP and central GeoLite verified; public aggregates contain 2 listeners; existing WASM persists GB/London history")
finally:
    # Only this randomly named project's resources are removed.
    for name in clients:
        subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    subprocess.run([*DC, "down", "--volumes", "--remove-orphans"], check=True)
    # Only remove this test run after its containers and volumes are gone.
    assert WORK.resolve().parent == (ROOT / ".local/icecast/geotest").resolve()
    assert WORK.name == PROJECT and not WORK.is_symlink()
    shutil.rmtree(WORK)
