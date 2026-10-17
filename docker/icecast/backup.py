#!/usr/bin/env python3
"""Icecast V1 private backups and isolated restore rehearsal (Linux Docker host)."""
import argparse
import copy
import hashlib
import json
import os
import re
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[2]
LOCAL = ROOT / ".local/icecast"
DC = ["docker", "compose", "-f", str(ROOT / "docker/icecast/compose.yaml")]
LIMIT = 1024 * 1024 * 1024
RECOVERY_IMAGE = "stationd-v1-icecast-recovery:dev"
CENTRAL_SERVICES = ("icecast", "admin", "router")


def run(args, **kw):
    return subprocess.run(args, check=True, capture_output=True, **kw).stdout


def inspect(service, required=True):
    cid = run([*DC, "ps", "--all", "-q", service], text=True).strip()
    if not cid:
        if required: raise RuntimeError(f"central service has no container: {service}")
        return None
    return json.loads(run(["docker", "inspect", cid], text=True))[0]


def recovery_image(reference=RECOVERY_IMAGE):
    return json.loads(run(["docker", "image", "inspect", reference], text=True))[0]["Id"]


def central_deployment(deployment, services):
    result = copy.deepcopy(deployment)
    result["services"] = {name: result["services"][name] for name in services}
    used = {v["source"] for service in result["services"].values() for v in service.get("volumes", []) if v["type"] == "volume"}
    result["volumes"] = {name: value for name, value in result.get("volumes", {}).items() if name in used}
    return result


def copy_tree(source_root, target_root, originals):
    for source in sorted(source_root.rglob("*")):
        if source.is_symlink(): raise RuntimeError("symlinks are not supported in central data")
        if not source.is_file(): continue
        target = target_root / source.relative_to(source_root)
        target.parent.mkdir(parents=True, exist_ok=True)
        originals[source] = source.read_bytes()
        target.write_bytes(originals[source])


def helper(image, mounts, code, *args):
    command = ["docker", "run", "--rm", "--network", "none", "--read-only",
               "--cap-drop", "ALL", "--cap-add", "DAC_OVERRIDE", "--cap-add", "CHOWN",
               "--security-opt", "no-new-privileges:true"]
    for mount in mounts:
        command += ["--mount", mount]
    return run([*command, "--entrypoint", "python3", image, "-c", code, *args])


def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def private_work():
    LOCAL.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(prefix="recovery-", dir=LOCAL)


def create():
    directory = LOCAL / "backups"
    directory.mkdir(mode=0o700, exist_ok=True)
    directory.chmod(0o700)
    deployment = json.loads(run([*DC, "--profile", "*", "config", "--format", "json"], text=True))
    owners = {name: container for name in CENTRAL_SERVICES if name in deployment["services"]
            if (container := inspect(name, required=name != "router")) is not None}
    if not {"icecast", "admin"} <= owners.keys(): raise RuntimeError("central Icecast/admin services are required")
    deployment = central_deployment(deployment, owners)
    image = recovery_image()
    with private_work() as temporary:
        stage = Path(temporary)
        (stage / "config").mkdir()
        (stage / "secrets").mkdir(mode=0o700)
        (stage / "geoip").mkdir()
        (stage / "runtime/admin").mkdir(parents=True)
        (stage / "runtime/router").mkdir()
        originals = {}
        copy_tree(ROOT / "config", stage / "config", originals)
        copy_tree(LOCAL / "secrets", stage / "secrets", originals)
        for definition in deployment["secrets"].values():
            source = Path(definition["file"])
            if not source.resolve().is_relative_to((LOCAL / "secrets").resolve()) or source not in originals:
                raise RuntimeError("Compose secret is outside the captured central secrets tree")
        cfg = tomllib.loads(originals[ROOT / "config/icecast/icecast.toml"].decode())
        if cfg.get("geoip", {}).get("enabled"):
            source = ROOT / "data/geoip" / cfg["geoip"]["database_file"]
            if source.parent.resolve() != (ROOT / "data/geoip").resolve() or source.is_symlink():
                raise RuntimeError("unexpected GeoIP source")
            before = sha(source)
            shutil.copyfile(source, stage / "geoip" / source.name)
            if sha(source) != before or sha(stage / "geoip" / source.name) != before:
                raise RuntimeError("GeoIP changed during backup")
        loaded = run([*DC, "exec", "-T", "admin", "stationctl", "icecast", "config", "show"])
        if loaded != originals[ROOT / "config/icecast/icecast.toml"]:
            raise RuntimeError("disk Icecast configuration differs from active configuration")
        admin_volume = next(m["Name"] for m in owners["admin"]["Mounts"] if m["Destination"] == "/var/lib/station-admin")
        code = """import sqlite3,pathlib,os,time,shutil,sys
source=pathlib.Path('/source'); dest=pathlib.Path('/out'); deadline=time.monotonic()+60
files=list(source.rglob('*')); databases=set()
for path in files:
 if path.is_symlink(): raise RuntimeError('symlink in administrative state')
 if path.is_file():
  with path.open('rb') as f:
   if f.read(16)==bytes.fromhex('53514c69746520666f726d6174203300'): databases.add(path)
sidecars={pathlib.Path(str(p)+suffix) for p in databases for suffix in ('-wal','-shm','-journal')}
for path in files:
 if not path.is_file() or path in sidecars: continue
 target=dest/path.relative_to(source); target.parent.mkdir(parents=True,exist_ok=True)
 if path in databases:
  def progress(status,remaining,total):
   if time.monotonic()>deadline: raise RuntimeError('SQLite backup deadline exceeded')
  with sqlite3.connect('file:'+str(path)+'?mode=ro',uri=True,timeout=10) as src, sqlite3.connect(target) as dst:
   src.backup(dst,pages=256,progress=progress)
   assert dst.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
 else:
  before=path.read_bytes(); target.write_bytes(before)
  if path.read_bytes()!=before: raise RuntimeError('administrative state changed during backup')
 os.chmod(target,0o600); os.chown(target,int(sys.argv[1]),int(sys.argv[2]))
for directory in sorted(dest.rglob('*'),reverse=True):
 if directory.is_dir():
  os.chmod(directory,0o700); os.chown(directory,int(sys.argv[1]),int(sys.argv[2]))
"""
        helper(image, [f"type=volume,src={admin_volume},dst=/source,readonly", f"type=bind,src={stage / 'runtime/admin'},dst=/out"], code, str(os.getuid()), str(os.getgid()))
        if "router" in owners:
            router_volume = next(m["Name"] for m in owners["router"]["Mounts"] if m["Destination"] == "/var/lib/station-router")
            selection = helper(image, [f"type=volume,src={router_volume},dst=/source,readonly"], "from pathlib import Path; import sys; sys.stdout.buffer.write(Path('/source/selection.json').read_bytes())")
            json.loads(selection)
            (stage / "runtime/router/selection.json").write_bytes(selection)
        for source, original in originals.items():
            if source.read_bytes() != original:
                raise RuntimeError("configuration or secrets changed during backup")
        (stage / "deployment.json").write_text(json.dumps(deployment))
        files = {p.relative_to(stage).as_posix(): {"sha256": sha(p), "size": p.stat().st_size} for p in stage.rglob("*") if p.is_file()}
        manifest = {"format": 2, "created_at": int(time.time()), "git_commit": run(["git", "rev-parse", "HEAD"], text=True).strip(),
                    "images": {s: c["Image"] for s, c in owners.items()}, "tools_image": image, "files": files,
                    "secret_files": {name: Path(value["file"]).relative_to(LOCAL / "secrets").as_posix() for name, value in deployment["secrets"].items()},
                    "notes": "Live SQLite backup; router selection captured separately; no playback state. Archive contains secrets."}
        metadata = json.dumps(manifest, indent=2).encode()
        if len(files) + 1 > 16384 or len(metadata) > 8 * 1024 * 1024 or sum(f["size"] for f in files.values()) + len(metadata) > LIMIT:
            raise RuntimeError("backup exceeds restore inventory limits; no archive created")
        (stage / "manifest.json").write_bytes(metadata)
        destination = directory / (time.strftime("icecast-%Y%m%d-%H%M%S-") + uuid.uuid4().hex[:8] + ".tar.gz")
        partial = destination.with_suffix(".partial")
        try:
            with partial.open("xb") as output:
                os.chmod(partial, 0o600)
                with tarfile.open(fileobj=output, mode="w:gz", compresslevel=1) as archive:
                    for p in sorted(stage.rglob("*")):
                        if p.is_file():
                            archive.add(p, arcname=p.relative_to(stage).as_posix(), recursive=False)
                output.flush(); os.fsync(output.fileno())
            os.replace(partial, destination)
        finally:
            partial.unlink(missing_ok=True)
        print(destination)
        return destination


def extract(archive_path, destination):
    # No tar.extractall: exact members, regular files only, bounded sizes and checksums.
    destination = destination.resolve()
    if destination.exists():
        raise RuntimeError("restore destination must not exist")
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
        names = [m.name for m in members]
        if len(names) != len(set(names)) or len(names) > 16384 or sum(m.size for m in members) > LIMIT:
            raise RuntimeError("invalid archive inventory")
        for m in members:
            p = PurePosixPath(m.name)
            if not m.isfile() or p.is_absolute() or ".." in p.parts or "\\" in m.name or not p.parts:
                raise RuntimeError("unsafe archive member")
        metadata = archive.getmember("manifest.json")
        if metadata.size > 8 * 1024 * 1024:
            raise RuntimeError("manifest too large")
        manifest = json.load(archive.extractfile(metadata))
        if manifest.get("format") not in (1, 2) or set(names) != {"manifest.json", *manifest["files"]}:
            raise RuntimeError("manifest does not match archive")
        allowed = ("config/", "secrets/", "geoip/", "runtime/admin/", "runtime/router/")
        if any(n != "deployment.json" and not n.startswith(allowed) for n in manifest["files"]):
            raise RuntimeError("unexpected backup content")
        destination.mkdir(mode=0o700, parents=True)
        try:
            for m in members:
                target = destination / m.name
                target.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
                with archive.extractfile(m) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o600 if m.name.startswith(("secrets/", "runtime/")) or m.name == "manifest.json" else 0o644)
                if m.name != "manifest.json" and (target.stat().st_size != manifest["files"][m.name]["size"] or sha(target) != manifest["files"][m.name]["sha256"]):
                    raise RuntimeError("backup member checksum mismatch")
            for p in destination.rglob("*"):
                if p.is_dir(): p.chmod(0o755)
            # Individual file binds bypass this private secrets parent.
            (destination / "secrets").chmod(0o700)
            for p in (destination / "secrets").iterdir(): p.chmod(0o644)
        except BaseException:
            shutil.rmtree(destination)
            raise
    return manifest


def rehearsal(archive_path, stream_probes=False):
    project = "stationd-restore-" + uuid.uuid4().hex[:10]
    with private_work() as temporary:
        root = Path(temporary) / "restored"
        manifest = extract(archive_path, root)
        tool_image = manifest.get("tools_image") or recovery_image()
        images = {name: image for name, image in manifest["images"].items() if name in CENTRAL_SERVICES}
        if not {"icecast", "admin"} <= images.keys(): raise RuntimeError("central service images missing")
        if manifest["format"] == 2 and set(images) != set(manifest["images"]): raise RuntimeError("unsupported central data owner")
        for image in {*images.values(), tool_image}:
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
                raise RuntimeError("backup images must be immutable local Docker image IDs")
            run(["docker", "image", "inspect", image])
        deployment = json.loads((root / "deployment.json").read_text())
        deployment["name"] = project
        deployment["networks"] = {"default": {"name": project + "-default"}}
        deployment = central_deployment(deployment, images)
        config_root = root / "config/icecast" if manifest["format"] == 2 else root / "config"
        for name, service in deployment["services"].items():
            service.pop("build", None); service.pop("profiles", None)
            service["image"] = images[name]
            service["pull_policy"] = "never"
            service["ports"] = []
            for mount in service.get("volumes", []):
                if mount["type"] == "bind":
                    target = mount["target"]
                    mapping = {"/etc/stationd": config_root, "/etc/stationd/router.toml": config_root / "router.toml", "/data/geoip": root / "geoip"}
                    if target not in mapping: raise RuntimeError("unexpected bind in backup")
                    mount["source"] = str(mapping[target])
        for name, secret in deployment["secrets"].items():
            relative = PurePosixPath(manifest.get("secret_files", {}).get(name, Path(secret["file"]).name))
            if relative.is_absolute() or ".." in relative.parts or "secrets/" + str(relative) not in manifest["files"]:
                raise RuntimeError("invalid archived secret mapping")
            secret["file"] = str(root / "secrets" / relative)
        deployment["volumes"] = {name: {"name": project + "-" + name} for name in deployment["volumes"]}
        if stream_probes:
            add_stream_probes(deployment, config_root, tool_image)
        cfg = root / "restore.json"; cfg.write_text(json.dumps(deployment))
        dc = ["docker", "compose", "-p", project, "-f", str(cfg)]
        try:
            for role in (role for role in ("admin", "router") if role in images):
                (root / "runtime" / role).mkdir(parents=True, exist_ok=True)
                volume = project + "-" + role + "-state"
                run(["docker", "volume", "create", "--label", "stationd.restore-test=" + project, volume])
                restored_checks = json.loads(helper(tool_image, [f"type=volume,src={volume},dst=/out", f"type=bind,src={root / 'runtime' / role},dst=/input,readonly"],
                    RESTORE_FILES,
                    "legacy-admin" if role == "admin" and manifest["format"] == 1 else role))
                for name, checksum in restored_checks.items():
                    if checksum != manifest["files"][f"runtime/{role}/{name}"]["sha256"]:
                        raise RuntimeError("restored runtime file differs from archived snapshot")
            run([*dc, "up", "-d", "--wait", *deployment["services"]])
            def ctl(*args):
                return json.loads(run([*dc, "exec", "-T", "admin", "stationctl", "icecast", *args], text=True))
            selected = "no router"
            if "router" in images:
                expected = json.loads((root / "runtime/router/selection.json").read_text())
                route = ctl("route")
                if route["input"] != expected["input"]: raise RuntimeError("router selection not restored")
                selected = route["input"]
            if stream_probes:
                expected_mounts = {m["name"] for m in tomllib.loads((config_root / "icecast.toml").read_text())["mounts"]}
                deadline = time.monotonic() + 45
                while not expected_mounts <= {m["mount"] for m in ctl("status")["mounts"] if m["connected"]}:
                    if time.monotonic() > deadline: raise RuntimeError("restored mount probes did not connect")
                    time.sleep(.5)
            command = deployment["services"]["admin"].get("command", [])
            if any(arg == "--settings" or arg.startswith("--settings=") for arg in command):
                status = ctl("history-status")
                if any(plugin["problem"] for plugin in status["plugins"]):
                    raise RuntimeError("restored administrative plugin reported an error")
            for name in deployment["services"]:
                cid = run([*dc, "ps", "-q", name], text=True).strip()
                if any(json.loads(run(["docker", "inspect", cid], text=True))[0]["NetworkSettings"]["Ports"].values()):
                    raise RuntimeError("restore test unexpectedly published ports")
            print("PASS: restored config and secrets authenticate; central data restored; stream probes=" + str(stream_probes) + "; selection", selected, "; administrative data verified; no published ports")
        finally:
            run([*dc, "down", "--volumes", "--remove-orphans"])
            for role in (role for role in ("admin", "router") if role in images):
                subprocess.run(["docker", "volume", "rm", project + "-" + role + "-state"], capture_output=True)



RESTORE_FILES = """import pathlib,shutil,os,hashlib,json,sys
src=pathlib.Path('/input')
dst=pathlib.Path('/out')/('plugins' if sys.argv[1]=='legacy-admin' else '')
dst.mkdir(exist_ok=True)
checks={}
for path in src.rglob('*'):
 if not path.is_file(): continue
 target=dst/path.relative_to(src)
 target.parent.mkdir(parents=True,exist_ok=True)
 shutil.copyfile(path,target)
 checks[path.relative_to(src).as_posix()]=hashlib.sha256(target.read_bytes()).hexdigest()
for path in sorted(dst.rglob('*'),reverse=True):
 os.chmod(path,0o700 if path.is_dir() else 0o600)
 os.chown(path,65534,65534)
os.chmod(dst,0o700)
os.chown(dst,65534,65534)
print(json.dumps(checks))
"""

STREAM_PROBE = """import os,pathlib,urllib.parse
secret=pathlib.Path('/run/secrets/probe-password').read_text().strip()
target='icecast://source:'+urllib.parse.quote(secret,safe='')+'@icecast:8000'+os.environ['MOUNT']
os.execvp('ffmpeg',['ffmpeg','-nostdin','-hide_banner','-loglevel','quiet','-re','-f','lavfi','-i','sine=frequency=440:sample_rate=44100','-af','volume=0.15','-ac','1','-c:a','libmp3lame','-b:a','64k','-reservoir','0','-content_type','audio/mpeg','-f','mp3',target])
"""


def add_stream_probes(deployment, config_root, image):
    config = tomllib.loads((config_root / "icecast.toml").read_text())
    outputs = set()
    if "router" in deployment["services"]:
        outputs.add(tomllib.loads((config_root / "router.toml").read_text())["output_mount"])
    credentials = {Path(s.get("target", s["source"])).name: s["source"] for s in deployment["services"]["icecast"]["secrets"]}
    for index, mount in enumerate(m for m in config["mounts"] if m["name"] not in outputs):
        secret = credentials.get(mount["password_file"])
        if secret is None:
            raise RuntimeError("mount password is not provisioned in Icecast Compose secrets")
        deployment["services"][f"restore-probe-{index}"] = {
            "image":image,"pull_policy":"never","entrypoint":["python3","-c",STREAM_PROBE],
            "environment":{"MOUNT":mount["name"]},
            "secrets":[{"source":secret,"target":"probe-password"}],
            "depends_on":{"icecast":{"condition":"service_healthy"}},
            "read_only":True,"cap_drop":["ALL"],"security_opt":["no-new-privileges:true"]}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("create")
    p = sub.add_parser("extract"); p.add_argument("archive", type=Path); p.add_argument("destination", type=Path)
    p = sub.add_parser("test"); p.add_argument("archive", type=Path); p.add_argument("--stream-probes", action="store_true")
    args = parser.parse_args()
    if args.command == "create": create()
    elif args.command == "extract":
        extract(args.archive, args.destination); print("Verified and extracted into", args.destination)
    else: rehearsal(args.archive, args.stream_probes)


if __name__ == "__main__":
    os.umask(0o077)
    try: main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError, tarfile.TarError, KeyError) as error:
        raise SystemExit("Icecast recovery failed: " + str(error))
