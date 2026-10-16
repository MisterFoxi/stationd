#!/usr/bin/env python3
"""Real DNS/TLS/Traefik/Icecast path, effective IP and decoded audio.
Run on stationdev1 with the live switch bench already running.
"""
import json
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml")]
BASE = "https://v1stream.homestone.rp-radio.live"
UA = "stationd-v1-traefik-e2e-" + uuid.uuid4().hex[:10]


def ctl(*args):
    return json.loads(subprocess.check_output([*DC, "exec", "-T", "admin", "stationctl", "icecast", *args], text=True))


with urllib.request.urlopen(BASE + "/status-json.xsl", timeout=8) as response:
    assert json.load(response)["icestats"]["host"] == "stationdev1.lan"
try:
    urllib.request.urlopen(BASE + "/admin/", timeout=8)
    raise AssertionError("anonymous administration unexpectedly allowed")
except urllib.error.HTTPError as error:
    assert error.code == 401, error.code

# An untrusted public request must not choose its own geographic IP.
request = urllib.request.Request(BASE + "/", headers={"User-Agent": UA, "X-Forwarded-For": "81.2.69.160"})
with urllib.request.urlopen(request, timeout=10) as stream:
    deadline = time.monotonic() + 8
    while True:
        matches = [l for l in ctl("listeners", "/radio.mp3") if l["user_agent"] == UA]
        if matches:
            break
        assert time.monotonic() < deadline, "forwarded listener missing"
        time.sleep(.25)
    listener = matches[0]
    assert listener["ip"] not in ("192.168.1.94", "81.2.69.160"), listener
    audio = stream.read(16384)
    assert len(audio) == 16384
    decode = subprocess.run([*DC, "exec", "-T", "live-a", "ffmpeg", "-v", "error", "-f", "mp3", "-i", "pipe:0", "-f", "null", "-"], input=audio, capture_output=True, timeout=15)
    assert decode.returncode == 0, decode.stderr.decode()
    print("PASS: public DNS + valid TLS + root audio route + native admin authentication; effective client IP", listener["ip"], "; forged forwarded header refused; MP3 decoded")

# Second Life receives direct HTTP audio, with no redirect to HTTPS.
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


plain = urllib.request.build_opener(NoRedirect())
http_base = BASE.replace("https://", "http://", 1)
for path in ("/", "/radio.mp3"):
    with plain.open(http_base + path, timeout=10) as stream:
        assert stream.status == 200 and stream.geturl() == http_base + path
        assert stream.headers.get_content_type() == "audio/mpeg"
        audio = stream.read(16384)
        assert len(audio) == 16384
        decode = subprocess.run([*DC, "exec", "-T", "live-a", "ffmpeg", "-v", "error", "-f", "mp3", "-i", "pipe:0", "-f", "null", "-"], input=audio, capture_output=True, timeout=15)
        assert decode.returncode == 0, decode.stderr.decode()
try:
    plain.open(http_base + "/admin/", timeout=8)
    raise AssertionError("HTTP administration did not redirect")
except urllib.error.HTTPError as error:
    assert error.code in (301, 302, 307, 308), error.code
    assert error.headers["Location"] == BASE + "/admin/"
print("PASS: Second Life HTTP root and /radio.mp3 stream MP3 without redirect; administration redirects to HTTPS")
