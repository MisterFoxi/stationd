#!/usr/bin/env python3
"""Validate, apply and roll back central Icecast TOML on stationdev1."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
CANONICAL = ROOT / "config/icecast/icecast.toml"
WORK = ROOT / ".local/icecast/apply"
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml"),
      "--profile", "live", "--profile", "switch"]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def compose(*args, **kwargs):
    return subprocess.run([*DC, *args], cwd=ROOT, check=True, **kwargs)


def ctl(*args):
    return compose("exec", "-T", "admin", "stationctl", "icecast", *args,
                   capture_output=True).stdout


def atomic_write(path, data, mode=0o644):
    fd, name = tempfile.mkstemp(prefix=".icecast-", suffix=".tmp", dir=path.parent)
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def running():
    return set(compose("ps", "--status", "running", "--services",
                       capture_output=True, text=True).stdout.split())


def validate_router(candidate, services):
    if "router" not in services:
        return
    cfg = tomllib.loads(candidate.decode())
    router = tomllib.loads((ROOT / "config/icecast/router.toml").read_text())
    mounts = {m["name"]: m for m in cfg["mounts"]}
    required = [router["output_mount"], *(i["mount"] for i in router["inputs"])]
    if not all(m in mounts for m in required):
        raise RuntimeError("active router requires its output and input mounts")
    if mounts[router["output_mount"]]["password_file"] != router["output_password_file"]:
        raise RuntimeError("active router output password reference must match Icecast")


def restart_services(services):
    compose("up", "-d", "--force-recreate", "--wait", "icecastd", "admin")
    fixtures = [s for s in ("live-a", "live-b", "router") if s in services]
    if fixtures:
        compose("up", "-d", "--no-deps", "--force-recreate", *fixtures)
        expected = {"/test-a.mp3" for s in fixtures if s == "live-a"}
        expected |= {"/test-b.mp3" for s in fixtures if s == "live-b"}
        if "router" in fixtures:
            expected.add(tomllib.loads((ROOT / "config/icecast/router.toml").read_text())["output_mount"])
        deadline = time.monotonic() + 30
        while True:
            status = json.loads(ctl("status"))
            connected = {m["mount"] for m in status["mounts"] if m["connected"]}
            if expected <= connected:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("test sources/router did not reconnect")
            time.sleep(0.5)


def apply_with_rollback(candidate, active, services, backup):
    atomic_write(backup, active, 0o600)
    atomic_write(CANONICAL, candidate)
    try:
        restart_services(services)
        loaded = ctl("config", "revision").decode().strip()
        if loaded != digest(candidate):
            raise RuntimeError("loaded revision does not match candidate")
    except BaseException:
        atomic_write(CANONICAL, active)
        try:
            restart_services(services)
        except BaseException as rollback_error:
            raise RuntimeError(f"application and rollback failed; previous config saved at {backup}") from rollback_error
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=CANONICAL)
    parser.add_argument("--check", action="store_true", help="validate without changing files or services")
    parser.add_argument("--reload", action="store_true", help="reload even when TOML is unchanged (GeoIP/secrets refresh)")
    parser.add_argument("--expect-revision", help="refuse if the running configuration changed")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("run this script on stationdev1")
    managed = compose('ps', '-q', 'icecastd', capture_output=True, text=True).stdout.strip()
    if managed:
        label = subprocess.run(
            ['docker', 'inspect', '--format', '{{ index .Config.Labels "stationdeck.configuration.managed" }}', managed],
            check=True, capture_output=True, text=True).stdout.strip()
        if label == 'true':
            parser.error('icecastd is managed by StationDeck; publish there or explicitly leave managed mode first')
    WORK.mkdir(parents=True, exist_ok=True, mode=0o700)
    WORK.chmod(0o700)
    with (WORK / "apply.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        candidate = args.config.read_bytes()
        if len(candidate) > 256 * 1024:
            raise RuntimeError("configuration exceeds 256 KiB")
        canonical_before = CANONICAL.read_bytes()
        # A single file bind bypasses the private parent directory, like Compose secrets.
        fd, staged = tempfile.mkstemp(prefix="candidate-", suffix=".toml", dir=WORK)
        os.close(fd)
        try:
            atomic_write(Path(staged), candidate)
            compose("run", "--rm", "--no-deps", "--entrypoint", "station-icecast-config",
                    "--volume", f"{staged}:/run/candidate.toml:ro",
                    "icecastd", "check", "/run/candidate.toml", "/run/secrets")
        finally:
            Path(staged).unlink(missing_ok=True)
        if args.check:
            print("Validated without restart:", digest(candidate))
            return
        active = ctl("config", "show")
        if args.expect_revision and digest(active) != args.expect_revision:
            raise RuntimeError("running configuration revision changed")
        if CANONICAL.read_bytes() != canonical_before:
            raise RuntimeError("configuration changed during validation; retry")
        services = running()
        validate_router(candidate, services)
        if candidate == active and not args.reload:
            print("Already active:", digest(candidate))
            return
        backup = WORK / (time.strftime("%Y%m%d-%H%M%S") + "-" + digest(active)[:12] + ".toml")
        apply_with_rollback(candidate, active, services, backup)
        print("Applied:", digest(candidate))
        print("Previous active configuration:", backup)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Icecast config not applied: {error}")

