#!/usr/bin/env python3
"""Archive extraction refuses traversal, links, duplicate members and corruption."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("icecast_backup", Path(__file__).resolve().parents[1] / "docker/icecast/backup.py")
backup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backup)


class ExtractionTests(unittest.TestCase):
    def make_archive(self, path, name="secrets/admin-password", content=b"test-only", digest=None, duplicate=False, link=False):
        manifest = {"format":1,"files":{name:{"size":len(content),"sha256":digest or hashlib.sha256(content).hexdigest()}}}
        with tarfile.open(path, "w:gz") as archive:
            data = json.dumps(manifest).encode()
            info = tarfile.TarInfo("manifest.json"); info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
            for _ in range(2 if duplicate else 1):
                info = tarfile.TarInfo(name); info.size = len(content)
                if link: info.type = tarfile.SYMTYPE; info.linkname = "/tmp/outside"
                archive.addfile(info, io.BytesIO(content))

    def test_valid_archive_private_root_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder); archive = root / "test.tar.gz"; dest = root / "restored"
            self.make_archive(archive)
            backup.extract(archive, dest)
            self.assertEqual((dest / "secrets/admin-password").read_bytes(), b"test-only")
            self.assertEqual(dest.stat().st_mode & 0o777, 0o700)
            self.assertEqual((dest / "secrets").stat().st_mode & 0o777, 0o700)
            with self.assertRaisesRegex(RuntimeError, "must not exist"):
                backup.extract(archive, dest)

    def test_corruption_is_refused_and_partial_extraction_removed(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder); archive = root / "test.tar.gz"; dest = root / "restored"
            self.make_archive(archive, digest="0" * 64)
            with self.assertRaisesRegex(RuntimeError, "checksum"):
                backup.extract(archive, dest)
            self.assertFalse(dest.exists())

    def test_traversal_and_links_are_refused_before_writing(self):
        for name, link in [("../outside",False),("/tmp/outside",False),("secrets/link",True)]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as folder:
                root = Path(folder); archive = root / "test.tar.gz"; dest = root / "restored"
                self.make_archive(archive,name=name,link=link)
                with self.assertRaisesRegex(RuntimeError, "unsafe"):
                    backup.extract(archive, dest)
                self.assertFalse(dest.exists())

    def test_duplicate_members_are_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder); archive = root / "test.tar.gz"; dest = root / "restored"
            self.make_archive(archive,duplicate=True)
            with self.assertRaisesRegex(RuntimeError, "inventory"):
                backup.extract(archive, dest)
            self.assertFalse(dest.exists())



class CentralScopeTests(unittest.TestCase):
    def test_old_and_new_archive_service_names(self):
        for service in ("icecast", "icecastd"):
            self.assertEqual(backup.archived_icecast_service({service: {}, "admin": {}}), service)
        for services in ({"admin": {}}, {"icecast": {}, "icecastd": {}}):
            with self.assertRaisesRegex(RuntimeError, "exactly one"):
                backup.archived_icecast_service(services)

    def test_legacy_stream_probe_uses_archived_dns(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "icecast.toml").write_text('[[mounts]]\nname="/test.mp3"\npassword_file="source-password"\n')
            for service in ("icecast", "icecastd"):
                deployment = {"services": {service: {"secrets": [{"source": "source-password"}]}, "admin": {}}}
                backup.add_stream_probes(deployment, root, "fixture:image")
                probe = deployment["services"]["restore-probe-0"]
                self.assertEqual(probe["environment"]["ICECAST_HOST"], service)
                self.assertEqual(set(probe["depends_on"]), {service})

    def test_station_count_does_not_define_central_services(self):
        deployment = {"services": {"icecastd":{},"admin":{},"router":{}, **{f"station-{i}":{} for i in range(100)}}, "volumes":{}}
        result = backup.central_deployment(deployment, ("icecastd","admin","router"))
        self.assertEqual(set(result["services"]), {"icecastd","admin","router"})
        self.assertEqual(len(deployment["services"]), 103)

    def test_stream_probes_follow_mounts_for_zero_one_three_and_many_sources(self):
        for count in (0,1,3,25,100):
            with self.subTest(count=count), tempfile.TemporaryDirectory() as folder:
                root=Path(folder)
                text='[[mounts]]\nname="/radio.mp3"\npassword_file="output-password"\n'
                secrets=[{"source":"output-password","target":"output-password"}]
                for index in range(count):
                    text+=f'[[mounts]]\nname="/station-{index}.mp3"\npassword_file="source-{index}"\n'
                    secrets.append({"source":f"source-{index}","target":f"/run/secrets/source-{index}"})
                (root/"icecast.toml").write_text(text)
                (root/"router.toml").write_text('output_mount="/radio.mp3"\n')
                deployment={"services":{"icecastd":{"secrets":secrets},"admin":{},"router":{}}}
                backup.add_stream_probes(deployment,root,"test-image")
                probes=[s for name,s in deployment["services"].items() if name.startswith("restore-probe-")]
                self.assertEqual(len(probes),count)
                self.assertEqual({p["environment"]["MOUNT"] for p in probes},{f"/station-{i}.mp3" for i in range(count)})
                self.assertTrue(all("ports" not in p for p in probes))

    def test_recursive_configurations_and_secrets_keep_their_paths(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder); source=root/"config"; target=root/"snapshot"
            for index in range(30):
                p=source/"stations"/str(index)/"station.toml"
                p.parent.mkdir(parents=True);p.write_text(f"id={index}")
            originals={}
            backup.copy_tree(source,target,originals)
            self.assertEqual(len(originals),30)
            self.assertEqual((target/"stations/29/station.toml").read_text(),"id=29")

if __name__ == "__main__": unittest.main()
