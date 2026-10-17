#!/usr/bin/env python3
"""Central deployment scope, dynamic secrets and bundle integrity."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


package = load('icecast_package', ROOT / 'docker/icecast/package.py')
installer = load('icecast_installer', ROOT / 'docker/icecast/deploy/install.py')


class DeploymentTests(unittest.TestCase):
    def test_only_central_services_and_portable_runtime_paths(self):
        service = {'build': {'context': '/development'}, 'profiles': ['test'],
                   'image': 'old:dev', 'secrets': [{'source': 'admin-password'}],
                   'volumes': [{'type': 'bind', 'source': '/development/config', 'target': '/etc/stationd', 'read_only': True},
                               {'type': 'bind', 'source': '/private/backups', 'target': '/central-backups', 'read_only': True},
                               {'type': 'volume', 'source': 'admin-state', 'target': '/central-admin'}]}
        original = {'services': {name: copy.deepcopy(service) for name in (*package.ROLES, 'smoke', 'live-a', 'stationd')},
                    'volumes': {'admin-state': {'name': 'development-admin-state'}},
                    'networks': {'default': {'name': 'development-network'}}}
        images = {'icecast': 'server:version', 'admin': 'admin:version', 'monitor': 'monitor:version'}
        result = package.runtime_compose(original, images)
        self.assertEqual(set(result['services']), set(package.ROLES))
        self.assertEqual(result['services']['router']['image'], images['admin'])
        for service in result['services'].values():
            self.assertNotIn('build', service)
            self.assertNotIn('profiles', service)
            self.assertEqual(service['pull_policy'], 'never')
            self.assertFalse(any(v['target'] == '/central-backups' for v in service['volumes']))
            self.assertTrue(all(v['source'].startswith('./') for v in service['volumes'] if v['type'] == 'bind'))
        self.assertEqual(result['volumes']['admin-state'], {})
        self.assertNotIn('name', result['networks']['default'])
        self.assertIn('stationd', original['services'])

    def test_unexpected_host_bind_is_refused(self):
        original = {'services': {name: {'volumes': [{'type': 'bind', 'source': '/media', 'target': '/media'}]} for name in package.ROLES}}
        with self.assertRaisesRegex(ValueError, 'unexpected central bind'):
            package.runtime_compose(original, dict.fromkeys(('icecast', 'admin', 'monitor'), 'image:version'))

    def test_secret_plan_follows_target_mounts(self):
        for count in (0, 1, 7, 100):
            with self.subTest(mounts=count):
                deployment = {'services': {name: {} for name in package.ROLES}}
                config = {'admin_password_file': 'site-admin', 'mounts': [{'password_file': f'local-source-{n}'} for n in range(count)]}
                result = installer.secret_plan(deployment, config, {'output_password_file': 'site-output'})
                self.assertEqual(set(result['services']['icecast']['secrets']), {'site-admin', *(f'local-source-{n}' for n in range(count))})
                self.assertEqual(len(result['secrets']), count + 4)
                self.assertEqual(result['services']['admin']['secrets'][0], 'site-admin')
                self.assertTrue(all(v['file'].startswith('./.local/icecast/secrets/') for v in result['secrets'].values()))

    def test_control_tokens_and_invalid_secret_paths_are_refused(self):
        deployment = {'services': {name: {} for name in package.ROLES}}
        for name in ('../escape', 'admin-api-token', 'router-api-token'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                installer.secret_plan(deployment, {'admin_password_file': 'admin-password', 'mounts': [{'password_file': name}]}, {'output_password_file': 'output-password'})
        with self.assertRaises(ValueError):
            installer.secret_plan(deployment, {'admin_password_file': 'admin-password', 'mounts': []}, {'output_password_file': 'admin-api-token'})

    def test_checksums_detect_corruption_and_unlisted_payload(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            payload = root / 'VERSION'
            payload.write_bytes(b'test-version\n')
            (root / 'SHA256SUMS').write_text(hashlib.sha256(payload.read_bytes()).hexdigest() + '  VERSION\n')
            installer.verify_bundle(root)
            payload.write_bytes(b'changed\n')
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                installer.verify_bundle(root)
            payload.write_bytes(b'test-version\n')
            (root / 'extra').write_text('unexpected')
            with self.assertRaisesRegex(ValueError, 'inventory mismatch'):
                installer.verify_bundle(root)

    def test_checksums_refuse_paths_outside_bundle_and_symlinks(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'SHA256SUMS').write_text('0' * 64 + '  ../outside\n')
            with self.assertRaisesRegex(ValueError, 'inventory'):
                installer.verify_bundle(root)
            (root / 'VERSION').symlink_to('/etc/passwd')
            (root / 'SHA256SUMS').write_text('0' * 64 + '  VERSION\n')
            with self.assertRaisesRegex(ValueError, 'bundle member'):
                installer.verify_bundle(root)


class InstallerFailureTests(unittest.TestCase):
    def fixture(self, root):
        bundle = root / 'bundle'
        config = bundle / 'config/icecast'
        config.mkdir(parents=True)
        (bundle / 'data/geoip').mkdir(parents=True)
        (config / 'icecast.toml').write_text('admin_password_file="admin-password"\nmounts=[]\n')
        (config / 'router.toml').write_text('output_password_file="output-password"\n')
        deployment = {'services': {name: {'image': 'server:version'} for name in package.ROLES}}
        (bundle / 'compose.yaml').write_text(json.dumps(deployment))
        (bundle / 'manifest.json').write_text(json.dumps({'tag': 'test-version', 'images': {'server:version': 'sha256:test'}}))
        for name in ('compose', 'ctl'):
            (bundle / name).write_text('#!/bin/sh\n')
        return bundle

    def call(self, bundle, node, fail=False, no_start=False):
        def invoke(args):
            if fail and 'up' in args:
                raise subprocess.CalledProcessError(1, args)

        def output(args, **kwargs):
            return '' if 'ps' in args else 'sha256:test\n'

        argv = ['install.py', '--dir', str(node), '--project', 'dist-unit-test']
        if no_start:
            argv.append('--no-start')
        with mock.patch.object(installer, '__file__', str(bundle / 'install.py')), \
             mock.patch.object(installer, 'verify_bundle'), \
             mock.patch.object(installer, 'invoke', side_effect=invoke), \
             mock.patch.object(installer.subprocess, 'check_output', side_effect=output), \
             mock.patch.object(installer.subprocess, 'run'), \
             mock.patch.object(sys, 'argv', argv):
            installer.main()

    def test_first_start_failure_retains_files_for_diagnosis(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            bundle = self.fixture(root)
            node = root / 'node'
            with self.assertRaises(subprocess.CalledProcessError):
                self.call(bundle, node, fail=True)
            self.assertTrue((node / 'compose.yaml').is_file())
            self.assertTrue((node / '.env').is_file())
            self.assertTrue((node / 'compose').is_file())
            self.assertFalse((node / '.compose.next.json').exists())

    def test_update_failure_restores_previous_runtime_and_preserves_secrets(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            bundle = self.fixture(root)
            node = root / 'node'
            self.call(bundle, node, no_start=True)
            previous = b'{"services":{"icecast":{"image":"previous:version"}}}'
            previous_env = b'ICECAST_BIND=127.0.0.1\nICECAST_PORT=18080\n'
            (node / 'compose.yaml').write_bytes(previous)
            (node / '.env').write_bytes(previous_env)
            token = node / '.local/icecast/secrets/admin-api-token'
            original_token = token.read_bytes()
            with self.assertRaises(subprocess.CalledProcessError):
                self.call(bundle, node, fail=True)
            self.assertEqual((node / 'compose.yaml').read_bytes(), previous)
            self.assertEqual((node / '.env').read_bytes(), previous_env)
            self.assertEqual(token.read_bytes(), original_token)


if __name__ == '__main__':
    unittest.main()
