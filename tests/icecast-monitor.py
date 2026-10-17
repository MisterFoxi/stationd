#!/usr/bin/env python3
"""Isolated monitoring faults: no source, StationD, Liquidsoap or published port."""
import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
DC = ['docker', 'compose', '-f', str(ROOT / 'docker/icecast/compose.yaml')]


def run(args, **kwargs):
    return subprocess.run(args, capture_output=True, text=True, **kwargs)


def main():
    result = run([*DC, '--profile', '*', 'config', '--format', 'json'], check=True)
    original = json.loads(result.stdout)
    project = 'stationd-monitor-test-' + uuid.uuid4().hex[:10]
    with tempfile.TemporaryDirectory(prefix=project + '-') as temporary:
        root = Path(temporary)
        root.chmod(0o755)
        config = root / 'config'
        shutil.copytree(ROOT / 'config/icecast', config)
        geo = root / 'geo'
        geo.mkdir()
        # The MMDB build epoch, not its freshly written mtime, must trigger age.
        fixture = 'https://raw.githubusercontent.com/maxmind/MaxMind-DB/0ee49278e35fd7bdf341ac236edbba20de1aceb9/test-data/GeoLite2-City-Test.mmdb'
        with urllib.request.urlopen(fixture, timeout=30) as response:
            data = response.read(2 * 1024 * 1024 + 1)
        assert 1024 < len(data) <= 2 * 1024 * 1024
        (geo / 'GeoLite2-City.mmdb').write_bytes(data)
        raw = (config / 'icecast.toml').read_text().split('[[mounts]]')[0]
        for n in range(7):
            raw += f'\n[[mounts]]\nname = "/watch-test-{n}.mp3"\npassword_file = "source-a-password"\naudience = true\n'
        (config / 'icecast.toml').write_text(raw)
        admin = (config / 'admin.toml').read_text().replace('sample_interval_seconds = 30', 'sample_interval_seconds = 5')
        (config / 'admin.toml').write_text(admin)
        # Deliberately expect a missing plugin only in the monitor's inventory.
        (config / 'expected-admin.toml').write_text(admin.replace('name = "listener-stats"', 'name = "missing-test-plugin"'))
        monitor = (config / 'monitor.toml').read_text()
        replacements = {
            'admin_config = "/etc/stationd/admin.toml"': 'admin_config = "/etc/stationd/expected-admin.toml"',
            'interval_seconds = 30': 'interval_seconds = 5',
            'grace_seconds = 60': 'grace_seconds = 0',
            'collection_max_age_seconds = 120': 'collection_max_age_seconds = 10',
            'geolite_max_age_days = 45': 'geolite_max_age_days = 1',
            'disk_min_available_bytes = 2147483648': 'disk_min_available_bytes = 9223372036854775807',
            'disk_paths = ["/central-backups", "/central-admin", "/central-router", "/data/geoip"]': 'disk_paths = ["/var/lib/station-monitor", "/missing-test-disk"]',
        }
        for before, after in replacements.items():
            assert before in monitor, before
            monitor = monitor.replace(before, after)
        (config / 'monitor.toml').write_text(monitor)
        (config / 'wrong-token').write_text('invalid-test-token')
        deployment = copy.deepcopy(original)
        deployment['name'] = project
        deployment['services'] = {name: deployment['services'][name] for name in ('icecast', 'admin', 'monitor')}
        deployment['networks'] = {'default': {'name': project + '-default'}}
        for name, service in deployment['services'].items():
            service.pop('build', None)
            service.pop('profiles', None)
            service['pull_policy'] = 'never'
            service['ports'] = []
            service['restart'] = 'no'
            if name == 'monitor':
                service['volumes'] = [v for v in service['volumes'] if v['target'] in ('/etc/stationd', '/data/geoip', '/var/lib/station-monitor')]
            for mount in service['volumes']:
                if mount['type'] == 'bind':
                    mount['source'] = str({'/etc/stationd': config, '/data/geoip': geo}[mount['target']])
        used = {v['source'] for s in deployment['services'].values() for v in s['volumes'] if v['type'] == 'volume'}
        deployment['volumes'] = {name: {'name': project + '-' + name} for name in used}
        path = root / 'compose.json'
        path.write_text(json.dumps(deployment))
        dc = ['docker', 'compose', '-p', project, '-f', str(path)]
        try:
            run([*dc, 'up', '-d', '--wait', 'icecast', 'admin'], check=True, timeout=120)
            run([*dc, 'up', '-d', '--no-deps', 'monitor'], check=True, timeout=30)

            def snapshot():
                response = run([*dc, 'exec', '-T', 'monitor', 'station-watch', '--check'])
                try:
                    return json.loads(response.stdout)
                except ValueError:
                    return {}

            def wait_for(predicate):
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    value = snapshot()
                    if predicate(value):
                        return value
                    time.sleep(1)
                raise AssertionError('monitor snapshot deadline: ' + json.dumps(snapshot()))

            expected = {f'source:/watch-test-{n}.mp3' for n in range(7)} | {
                'collection:missing-test-plugin', 'geolite:stale',
                'disk:low:/var/lib/station-monitor', 'disk:unavailable:/missing-test-disk',
            }
            value = wait_for(lambda v: expected <= set(v.get('active', [])))
            assert not value['checks']['admin:unavailable']
            assert not value['checks']['geolite:unavailable']
            assert value['details']['geolite_build_epoch'] < time.time() - 86400
            central = run([*dc, 'exec', '-T', 'admin', 'stationctl', 'icecast', 'monitoring'], check=True)
            assert expected <= set(json.loads(central.stdout)['active'])
            direct = run([*dc, 'exec', '-T', 'monitor', 'stationctl', '--addr', 'http://127.0.0.1:50063', 'monitor-status'], check=True)
            assert expected <= set(json.loads(direct.stdout)['active'])
            refused = run([*dc, 'exec', '-T', 'monitor', 'stationctl', '--addr', 'http://127.0.0.1:50063', '--token-file', '/etc/stationd/wrong-token', 'monitor-status'])
            assert refused.returncode != 0 and 'unauthenticated' in refused.stderr.lower()
            # Losing administration must not resolve any of these source alerts.
            run([*dc, 'stop', 'admin'], check=True, timeout=30)
            value = wait_for(lambda v: 'admin:unavailable' in v.get('active', []))
            assert expected <= set(value['active'])
            run([*dc, 'start', 'admin'], check=True, timeout=30)
            wait_for(lambda v: v.get('checks', {}).get('admin:unavailable') is False and 'admin:unavailable' not in v.get('active', []))
            # Real guest writes of unknown observations are not successful collection.
            (config / 'expected-admin.toml').write_text(admin)
            wait_for(lambda v: 'collection:listener-stats' in v.get('active', [])
                     and 'collection:missing-test-plugin' not in v.get('active', []))
            # Removing all audience mounts makes collection intentionally idle.
            (config / 'icecast.toml').write_text(raw.replace('audience = true', 'audience = false'))
            wait_for(lambda v: not any(key.startswith('collection:') and key != 'collection:status_unavailable' for key in v.get('active', []))
                     and 'collection:listener-stats' not in v.get('checks', {}))
            logs = run([*dc, 'logs', '--no-log-prefix', 'monitor'], check=True).stdout
            events = [json.loads(line) for line in logs.splitlines() if line.startswith('{')]
            assert sum(e.get('condition') == 'admin:unavailable' and e.get('event') == 'alert' for e in events) == 1
            assert sum(e.get('condition') == 'admin:unavailable' and e.get('event') == 'resolved' for e in events) == 1
            for name in deployment['services']:
                cid = run([*dc, 'ps', '-q', name], check=True).stdout.strip()
                state = json.loads(run(['docker', 'inspect', cid], check=True).stdout)[0]
                assert not any(state['NetworkSettings']['Ports'].values())
            print('PASS: seven dynamic missing sources; missing collection plugin; fresh-copy/old-build MMDB; low/unavailable disk; admin loss, dedup and recovery; intentionally idle collection; isolated network/volumes; no published ports or emitters')
        finally:
            run([*dc, 'down', '--volumes', '--remove-orphans'], check=True, timeout=60)


if __name__ == '__main__':
    main()
