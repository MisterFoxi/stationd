#!/usr/bin/env python3
"""Isolated real-audio test; no production daemon/Icecast/service is touched.
Run in the build container: python3 tests/liquidsoap_dj_preview.py
Requires cargo, Liquidsoap, ffmpeg. Uses the production Rust script generator.
"""
import array
import base64
import json
import math
import os
import pathlib
import socket
import sys
sys.dont_write_bytecode = True
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from liquidsoap_interrupt import tone, power, RATE

ROOT = pathlib.Path(__file__).resolve().parents[1]


def wait_for(check, timeout=15):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        if check():
            return
        time.sleep(.1)
    raise AssertionError("Timed out waiting for audio/transition")


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def tail_samples(path):
    data = path.read_bytes()[-RATE * 4:]
    data = data[:len(data) - len(data) % 4]
    samples = array.array("h", data)
    return samples[::2]


def has_tone(path, expected, absent):
    if not path.exists() or path.stat().st_size < RATE * 4:
        return False
    samples = tail_samples(path)[-8192:]
    return power(samples, expected) > 30 * max(1, power(samples, absent))


def main():
    state = {"take": False, "connected": False, "aired": 0, "disconnects": 0,
             "next": 0, "health": {}}
    with tempfile.TemporaryDirectory(prefix="stationd-dj-") as directory:
        work = pathlib.Path(directory)
        tone(work / "programme.wav", 440, 120)
        tone(work / "dj.wav", 880, 120)
        subprocess.run(["ffmpeg", "-v", "error", "-i", str(work / "dj.wav"),
                        "-codec:a", "libmp3lame", "-b:a", "128k", str(work / "dj.mp3")], check=True)

        class Bridge(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                assert self.headers.get("X-Stationd-Token") == "tok"
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
                route = self.path.removeprefix("/ls/v1/")
                response = {}
                if route == "next":
                    state["next"] += 1
                    response = {"kind": "file", "uri": str(work / "programme.wav"), "state": "", "reason": ""}
                elif route == "live/auth":
                    response = {"allow": body["user"] == "marc" and body["password"] == "s3cret"}
                elif route == "live/connect":
                    state["connected"] = True
                elif route == "live/health":
                    state["health"] = body
                    response = {"on_air": state["take"] and state["connected"]}
                elif route == "live/air":
                    state["aired"] += 1
                elif route == "live/disconnect":
                    state["connected"] = False
                    state["take"] = False
                    state["disconnects"] += 1
                    response = {"flush": False}
                data = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        bridge = ThreadingHTTPServer(("127.0.0.1", 0), Bridge)
        threading.Thread(target=bridge.serve_forever, daemon=True).start()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = dict(os.environ, STATIOND_DJ_TEST_DIR=directory,
                   STATIOND_DJ_BRIDGE=f"127.0.0.1:{bridge.server_port}", STATIOND_DJ_PORT=str(port))
        subprocess.run(["cargo", "test", "--locked", "--lib", "dj_audio_fixture", "--", "--ignored"],
                       cwd=ROOT, env=env, check=True)
        subprocess.run(["liquidsoap", "--check", str(work / "radio.liq")], check=True)
        receiver = sender = None
        log = (work / "liquidsoap.log").open("w")
        source_log = (work / "source.log").open("w")
        try:
            receiver = subprocess.Popen(["liquidsoap", str(work / "radio.liq")], stdout=log, stderr=log)
            wait_for(lambda: (work / "control.sock").exists())
            def source():
                return subprocess.Popen(["ffmpeg", "-v", "error", "-re", "-stream_loop", "-1",
                    "-i", str(work / "dj.mp3"), "-c:a", "copy", "-content_type", "audio/mpeg",
                    "-f", "mp3", f"icecast://marc:s3cret@127.0.0.1:{port}/live"],
                    stdout=source_log, stderr=source_log)
            sender = source()
            healthy = lambda: state["connected"] and state["health"].get("ready") and state["health"].get("rms", 0) > .05
            wait_for(healthy)
            time.sleep(4)
            assert state["aired"] == 0, "connecting for preview must not take the air"
            wait_for(lambda: has_tone(work / "air.wav", 440, 880))
            url = f"http://127.0.0.1:{port}/__stationd_preview"
            try:
                urllib.request.urlopen(url, timeout=3)
                raise AssertionError("Private preview allowed anonymous access")
            except urllib.error.HTTPError as error:
                assert error.code == 401
            req = urllib.request.Request(url, headers={"Authorization": "Basic " + base64.b64encode(b"stationd:tok").decode()})
            with urllib.request.urlopen(req, timeout=5) as stream:
                audio = stream.read(48000)
            subprocess.run(["ffmpeg", "-v", "error", "-f", "mp3", "-i", "pipe:0",
                            "-f", "wav", str(work / "preview.wav")], input=audio, check=True)
            assert has_tone(work / "preview.wav", 880, 440), "preview must contain DJ audio"
            assert has_tone(work / "air.wav", 440, 880), "preview leaked to the public air"
            before = state["next"]
            stop(sender)
            wait_for(lambda: state["disconnects"] == 1)
            time.sleep(1)
            assert state["next"] == before, "preview-only departure skipped the programme"
            sender = source()
            wait_for(healthy)
            state["take"] = True
            wait_for(lambda: state["aired"] == 1)
            wait_for(lambda: has_tone(work / "air.wav", 880, 440))
            stop(sender)
            wait_for(lambda: state["disconnects"] == 2)
            wait_for(lambda: has_tone(work / "air.wav", 440, 880))
            print("DJ audio: private preview, no pre-air leakage, no preview skip, explicit takeover and programme return passed.")
        except Exception:
            log.flush()
            source_log.flush()
            print((work / "liquidsoap.log").read_text()[-8000:])
            print((work / "source.log").read_text()[-2000:])
            raise
        finally:
            stop(sender)
            stop(receiver)
            bridge.shutdown()
            log.close()
            source_log.close()


if __name__ == "__main__":
    main()
