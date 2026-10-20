#!/usr/bin/env python3
"""Regression: local TOML/effective output must never bypass StationDeck startup."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('icecast_managed', ROOT / 'docker/stationdeck/icecast-managed.py')
managed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(managed)


class StartupTest(unittest.TestCase):
    def test_no_publication_or_cache_never_starts_local_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            effective = Path(directory) / 'icecast.toml'
            effective.write_text('location = "stale local output"\n')
            server = MagicMock()
            server.__enter__.return_value = server
            # End the supervisor when it reaches its first wait for a publication.
            server.accept.side_effect = KeyboardInterrupt
            agent = MagicMock()
            agent.poll.return_value = None
            with patch.object(managed, 'STOP', False), patch.object(managed, 'RoutingProxy'), \
                 patch.object(managed, 'EFFECTIVE', effective), \
                 patch.object(managed, 'SOCKET', str(Path(directory) / 'agent.sock')), \
                 patch.object(managed.signal, 'signal'), \
                 patch.object(managed.os, 'umask'), \
                 patch.object(managed.os, 'chmod'), \
                 patch.object(managed.socket, 'socket', return_value=server), \
                 patch.object(managed.subprocess, 'Popen', return_value=agent) as launch, \
                 patch.object(managed, 'start') as start, \
                 patch.object(managed, 'render') as render, \
                 patch.object(managed, 'atomic') as atomic, \
                 patch.object(managed, 'stop'):
                with self.assertRaises(KeyboardInterrupt):
                    managed.serve()
                launch.assert_called_once_with(['stationdeck-control', 'agent'])
                start.assert_not_called()
                render.assert_not_called()
                atomic.assert_not_called()
            self.assertIn('stale local output', effective.read_text())


if __name__ == '__main__':
    unittest.main()
