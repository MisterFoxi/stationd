#!/usr/bin/env python3
"""Restore an extended archive with extra station configs/mounts; never edits production."""
import argparse
import importlib.util
import json
from pathlib import Path
import tarfile

spec=importlib.util.spec_from_file_location("icecast_backup",Path(__file__).resolve().parents[1]/"docker/icecast/backup.py")
backup=importlib.util.module_from_spec(spec)
spec.loader.exec_module(backup)

p=argparse.ArgumentParser(description=__doc__)
p.add_argument("archive",type=Path)
p.add_argument("--extra-sources",type=int,default=3)
a=p.parse_args()
if not 1 <= a.extra_sources <= 20: p.error("test fixture accepts 1..20 extra sources")
with backup.private_work() as temporary:
    stage=Path(temporary)
    root=stage/"fixture"
    manifest=backup.extract(a.archive,root)
    if manifest["format"]!=2: raise RuntimeError("requires a central format-2 backup")
    config=root/"config/icecast/icecast.toml"
    router=root/"config/icecast/router.toml"
    for index in range(a.extra_sources):
        mount=f"/restore-extra-{index}.mp3"
        with config.open("a") as f:
            f.write(f'\n[[mounts]]\nname="{mount}"\npassword_file="source-a-password"\naudience=false\nhidden=true\n')
        with router.open("a") as f:
            f.write(f'\n[[inputs]]\nid="restore-extra-{index}"\nmount="{mount}"\n')
        station=root/"config/stations"/f"restore-extra-{index}"/"station.toml"
        station.parent.mkdir(parents=True)
        station.write_text(f'id="restore-extra-{index}"\nmount="{mount}"\n')
    manifest["files"]={f.relative_to(root).as_posix():{"sha256":backup.sha(f),"size":f.stat().st_size} for f in root.rglob("*") if f.is_file() and f.name!="manifest.json"}
    (root/"manifest.json").write_text(json.dumps(manifest))
    archive=stage/"extended.tar.gz"
    with tarfile.open(archive,"w:gz",compresslevel=1) as output:
        for f in root.rglob("*"):
            if f.is_file(): output.add(f,arcname=f.relative_to(root).as_posix(),recursive=False)
    backup.rehearsal(archive,stream_probes=True)
    print(f"PASS: {a.extra_sources} additional station configurations and audio sources restored from configuration")
