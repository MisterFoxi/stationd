#!/usr/bin/env python3
"""Regression checks for per-node IP refresh and mount-aware systemd units."""
import importlib.util
from pathlib import Path
import tomllib
import unittest


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parents[1] / "docker/prod" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


network = load("network", "network.py")
service = load("service", "service-unit.py")


class ServiceTest(unittest.TestCase):
    def test_each_node_and_changed_dhcp_preserve_settings_and_port(self):
        original = """[station]
name = "HomeStone"
[server] # node control
grpc_bind = '192.168.1.135:6000' # keep this port
[media]
library_path = "/mnt/nfs/radio"
"""
        for address in ("192.168.1.134", "192.168.2.42"):
            updated = network.configure(original, address)
            config = tomllib.loads(updated)
            self.assertEqual(config["server"]["grpc_bind"], address + ":6000")
            self.assertEqual(config["station"]["name"], "HomeStone")
            self.assertEqual(config["media"]["library_path"], "/mnt/nfs/radio")
            self.assertIn("# keep this port", updated)
            self.assertEqual(network.configure(updated, address), updated)

    def test_invalid_address_or_config_fails_without_replacement(self):
        for address in ("", "garbage", "0.0.0.0", "127.0.0.1", "224.0.0.1"):
            with self.subTest(address=address), self.assertRaises(ValueError):
                network.configure('[server]\ngrpc_bind = "192.168.1.1:50051"\n', address)
        for text in ("invalid TOML", '[server]\ngrpc_bind = "192.168.1.1:0"\n'):
            with self.assertRaises(ValueError):
                network.configure(text, "192.168.1.134")

    def test_mount_dependencies_and_node_directory(self):
        unit = service.unit("/opt/stationd", ["/mnt/nfs/radio", "/mnt/nfs/home/playlist", "/mnt/nfs/radio"])
        self.assertIn('RequiresMountsFor="/opt/stationd" "/mnt/nfs/radio" "/mnt/nfs/home/playlist"', unit)
        self.assertIn('ExecStart=/bin/bash "/opt/stationd/scripts/service.sh" start', unit)
        self.assertIn('ExecStop=/bin/bash "/opt/stationd/scripts/service.sh" stop', unit)
        self.assertIn("WantedBy=multi-user.target", unit)
        self.assertIn("After=docker.service network-online.target remote-fs.target", unit)

    def test_systemd_path_escaping(self):
        unit = service.unit('/opt/radio space %test $node', [])
        self.assertIn('"/opt/radio space %%test $node"', unit)
        self.assertIn('"/opt/radio space %%test $$node/scripts/service.sh"', unit)
        with self.assertRaises(ValueError):
            service.unit("/opt/radio\nInjected=bad", [])


if __name__ == "__main__":
    unittest.main()