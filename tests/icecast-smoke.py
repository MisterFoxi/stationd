#!/usr/bin/env python3
"""Real Icecast, synthetic sources + bounded listeners; no StationD or NFS."""
import base64
import json
import concurrent.futures
import os
import sys
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

BASE = os.environ.get("ICECAST_URL", "http://icecastd:8000").rstrip("/")
SECRETS = Path("/run/secrets")


def password(name):
    return (SECRETS / name).read_text().strip()


def request(path, user=None, secret=None, data=None, method=None):
    headers = {}
    if user is not None:
        token = base64.b64encode(f"{user}:{secret}".encode()).decode()
        headers["Authorization"] = f"Basic {token}"
    if method == "PUT":
        headers["Content-Type"] = "audio/mpeg"
    req = urllib.request.Request(BASE + path, headers=headers, data=data, method=method)
    return urllib.request.urlopen(req, timeout=5)


def stats():
    with request("/admin/stats", "admin", password("admin-password")) as response:
        return ET.fromstring(response.read())


def wait_for(predicate, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.2)
    raise AssertionError(f"Timeout: {description}")


def source_names():
    return {source.attrib["mount"] for source in stats().findall("source")}


def listener(mount, ready):
    with request(mount) as response:
        assert response.status == 200
        assert "audio/mpeg" in response.headers["Content-Type"]
        data = response.read(16384)
        assert len(data) == 16384, "stream ended before receiving enough audio"
        ready.set()
        # Keep the connection visible to Icecast's audience sampler.
        time.sleep(2)
    decoded = subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "mp3", "-i", "pipe:0",
         "-f", "null", "-"], input=data, stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE, timeout=10,
    )
    assert decoded.returncode == 0, "listener received undecodable MP3"
    return len(data)


def expect_denied(path, secret, method="PUT"):
    try:
        with request(path, "source", secret, data=b"" if method == "PUT" else None, method=method):
            raise AssertionError(f"unexpected access: {path}")
    except urllib.error.HTTPError as error:
        assert error.code == 401, f"expected 401, got {error.code}"



def admin(path, **parameters):
    query = urllib.parse.urlencode(parameters)
    with request("/admin/" + path + ("?" + query if query else ""),
                 "admin", password("admin-password")) as response:
        return response.read()


def administration():
    # The native UI must work, independently of StationD.
    with request("/admin/", "admin", password("admin-password")) as response:
        assert response.status == 200
        assert "html" in response.headers["Content-Type"]
        assert b"Icecast" in response.read()

    title = "Test administration & accents — été"
    result = ET.fromstring(admin("metadata", mount="/test-a.mp3", mode="updinfo",
                                 song=title, charset="UTF-8"))
    assert result.findtext("return") == "1", "metadata update refused"
    wait_for(lambda: any(s.attrib.get("mount") == "/test-a.mp3"
                         and s.findtext("title") == title
                         for s in stats().findall("source")), "metadata update")
    with request("/test-a.mp3") as response:
        assert len(response.read(1024)) == 1024
        clients = wait_for(
            lambda: ET.fromstring(admin("listclients", mount="/test-a.mp3")).findall(".//listener"),
            "administrative client list",
        )
        client_id = clients[0].findtext("id") or clients[0].findtext("ID")
        assert client_id, "missing listener ID"
        result = ET.fromstring(admin("killclient", mount="/test-a.mp3", id=client_id))
        assert result.findtext("return") == "1", "listener disconnection refused"
        wait_for(lambda: not ET.fromstring(admin("listclients", mount="/test-a.mp3"))
                 .findall(".//listener"), "listener disconnected")

    result = ET.fromstring(admin("killsource", mount="/test-a.mp3"))
    assert result.findtext("return") == "1", "source disconnection refused"
    wait_for(lambda: source_names() == {"/test-b.mp3"}, "source A disconnected; B still live")
    # Disconnecting A must not interrupt B's audio.
    with request("/test-b.mp3") as response:
        assert len(response.read(1024)) == 1024
    print("PASS: native admin UI, metadata, listener listing/kick, source kick; other source still streams.")


def ctl(*arguments, token_file="/run/secrets/admin-api-token", success=True):
    command = ["stationctl", "--addr", os.environ.get("STATION_ADMIN_URL", "http://admin:50061"),
               "--token-file", token_file, "icecast", *arguments]
    result = subprocess.run(command, capture_output=True, text=True, timeout=15)
    if success is True:
        assert result.returncode == 0, f"stationctl failed: {result.stderr}"
    elif success is False:
        assert result.returncode != 0, "invalid administrative request accepted"
    return result


def grpc_administration():
    status = json.loads(ctl("status").stdout)
    assert status["server_id"].startswith("Icecast")
    mounts = {m["mount"]: m for m in status["mounts"]}
    assert not mounts["/test-a.mp3"]["connected"]
    assert mounts["/test-b.mp3"]["connected"]
    invalid = Path("/tmp/invalid-admin-token")
    invalid.write_text("wrong-token\n")
    result = ctl("status", token_file=str(invalid), success=False)
    assert "Unauthenticated" in result.stderr
    result = ctl("kick-source", "/undeclared.mp3", success=False)
    assert "InvalidArgument" in result.stderr

    title = "gRPC stationctl — été & musique"
    ctl("metadata", "/test-b.mp3", title)
    wait_for(lambda: any(m["title"] == title for m in json.loads(ctl("status").stdout)["mounts"]),
             "gRPC metadata update")
    with request("/test-b.mp3") as response:
        assert len(response.read(1024)) == 1024
        clients = wait_for(lambda: json.loads(ctl("listeners", "/test-b.mp3").stdout),
                           "gRPC listener list")
        ctl("kick-listener", "/test-b.mp3", str(clients[0]["id"]))
        wait_for(lambda: not json.loads(ctl("listeners", "/test-b.mp3").stdout),
                 "gRPC listener disconnection")
    ctl("kick-source", "/test-b.mp3")
    wait_for(lambda: not source_names(), "gRPC source disconnection")
    assert not any(m["connected"] for m in json.loads(ctl("status").stdout)["mounts"])
    print("PASS: stationctl via authenticated gRPC: status, listeners, metadata, kicks, invalid token/mount refused.")


def continuity_source():
    auth = urllib.parse.quote(password("source-a-password"), safe="")
    target = BASE.replace("http://", f"icecast://source:{auth}@", 1) + "/test-a.mp3"
    return subprocess.call(
        ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error", "-re",
         "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
         "-t", "45", "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k",
         "-content_type", "audio/mpeg", "-f", "mp3", target],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )

def main():
    import threading
    router = ctl("route", success=None)
    assert router.returncode != 0, "Stop switching fixtures first: make icecast-switch-down"
    assert not source_names(), "test server already has active sources"
    expect_denied("/test-a.mp3", "wrong-password")
    expect_denied("/undeclared.mp3", password("source-a-password"))
    try:
        request("/admin/stats")
        raise AssertionError("admin stats exposed without authentication")
    except urllib.error.HTTPError as error:
        assert error.code == 401

    processes = []
    try:
        for label, frequency in (("a", 440), ("b", 880)):
            auth = urllib.parse.quote(password(f"source-{label}-password"), safe="")
            target = BASE.replace("http://", f"icecast://source:{auth}@", 1)
            target += f"/test-{label}.mp3"
            processes.append(subprocess.Popen(
                ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error", "-re",
                 "-f", "lavfi", "-i", f"sine=frequency={frequency}:sample_rate=44100",
                 "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k",
                 "-content_type", "audio/mpeg", "-f", "mp3", target],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            ))
        wait_for(lambda: source_names() == {"/test-a.mp3", "/test-b.mp3"}, "two live sources")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            signals = [threading.Event(), threading.Event()]
            jobs = [pool.submit(listener, mount, signal) for mount, signal in
                    zip(("/test-a.mp3", "/test-b.mp3"), signals)]
            wait_for(lambda: all(signal.is_set() for signal in signals), "listeners receiving audio")
            wait_for(lambda: all(int(s.findtext("listeners", "0")) >= 1
                                for s in stats().findall("source")), "audience counters")
            received = [job.result(timeout=15) for job in jobs]
        assert all(process.poll() is None for process in processes), "a source exited unexpectedly"
        wait_for(lambda: all(int(s.findtext("listeners", "0")) == 0
                            for s in stats().findall("source")), "audience cleanup")
        print(f"PASS: 2 sources, 2 listeners, MP3 decoded ({received}), audience, auth, undeclared mount refused.")
        administration()
        grpc_administration()
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
        for process in processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    wait_for(lambda: not source_names(), "source cleanup")
    print("PASS: test sources and listeners disconnected; Icecast remains available.")


if __name__ == "__main__":
    if '--continuity-source' in sys.argv:
        raise SystemExit(continuity_source())
    main()

