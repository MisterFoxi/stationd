#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import tomllib
import unittest

spec = importlib.util.spec_from_file_location("paths", Path(__file__).parents[1] / "docker/prod/paths.py")
paths = importlib.util.module_from_spec(spec)
spec.loader.exec_module(paths)


class PathsTest(unittest.TestCase):
    def test_toml_paths_and_database_parent(self):
        config = tomllib.loads("""
[media]
library_path = "/mnt/nfs/radio"
[playlist]
path = "/mnt/nfs/homestone/playlist"
[grid]
path = "./grid"
[database]
path = "/data/station/stationd.db"
""")
        rows = {row[0]: row for row in paths.plan(config, "/opt/stationd", "/ignored")}
        self.assertEqual(rows["playlist"][1:4], ("/mnt/nfs/homestone/playlist", "/mnt/nfs/homestone/playlist", "external"))
        self.assertEqual(rows["database"][1], "/data/station")
        self.assertEqual(rows["grid"][1:4], ("/opt/stationd/grid", "/var/lib/stationd/grid", "local"))

    def test_literal_dollar_quotes_spaces_and_missing_host_creation(self):
        value = "/mnt/nfs/DJ's $songs"
        row = next(paths.plan({"media": {"library_path": value}}, "/opt/stationd", "/ignored"))
        mount = json.loads(row[4].replace("$$", "$"))
        self.assertEqual(mount["source"], value)
        self.assertFalse(mount["bind"]["create_host_path"])

    def test_relative_media_and_absolute_container_root(self):
        rows = list(paths.plan({"media": {"library_path": "./media"}, "playlist": {"path": "/var/lib/stationd/playlist"}}, "/opt/stationd", "/ignored"))
        self.assertEqual(rows[0][1], "/opt/stationd/media")
        self.assertEqual(rows[1][1:4], ("/opt/stationd/playlist", "/var/lib/stationd/playlist", "local"))

    def test_unsafe_invalid_or_escaping_paths(self):
        for value in ("", "../outside", "/etc", "//etc", "/usr/local/bin", "/mnt/a\nb", 5):
            with self.subTest(value=value), self.assertRaises(ValueError):
                list(paths.plan({"playlist": {"path": value}}, "/opt/stationd", "/mnt/media"))


if __name__ == "__main__":
    unittest.main()
