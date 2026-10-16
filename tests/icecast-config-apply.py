#!/usr/bin/env python3
"""Application safety: restore the ACTIVE configuration after a failed deployment."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("icecast_apply", ROOT / "docker/icecast/apply-config.py")
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)


class ApplicationTest(unittest.TestCase):
    def test_failed_restart_restores_active_configuration_and_keeps_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            canonical = Path(directory) / "icecast.toml"
            backup = Path(directory) / "previous.toml"
            canonical.write_bytes(b"edited-on-disk")
            active = b"active-before-edit"
            with patch.object(app, "CANONICAL", canonical), patch.object(app, "restart_services", side_effect=[RuntimeError("unhealthy"), None]) as restart:
                with self.assertRaisesRegex(RuntimeError, "unhealthy"):
                    app.apply_with_rollback(b"candidate", active, {"icecast","admin"}, backup)
            self.assertEqual(canonical.read_bytes(), active)
            self.assertEqual(backup.read_bytes(), active)
            self.assertEqual(restart.call_count, 2)

    def test_loaded_revision_mismatch_rolls_back(self):
        with tempfile.TemporaryDirectory() as directory:
            canonical = Path(directory) / "icecast.toml"
            backup = Path(directory) / "previous.toml"
            canonical.write_bytes(b"active")
            with patch.object(app, "CANONICAL", canonical), patch.object(app, "restart_services") as restart, patch.object(app, "ctl", return_value=b"wrong-revision"):
                with self.assertRaisesRegex(RuntimeError, "loaded revision"):
                    app.apply_with_rollback(b"candidate", b"active", set(), backup)
            self.assertEqual(canonical.read_bytes(), b"active")
            self.assertEqual(restart.call_count, 2)


if __name__ == "__main__":
    unittest.main()

