#!/usr/bin/env python3
"""Build the central V1 deployment bundle; optional delivery through docker/deploy.sh."""
import argparse
import copy
import datetime
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
ROLES = ('icecast', 'admin', 'router', 'monitor')


def run(args):
    return subprocess.check_output(args, text=True).strip()


def runtime_compose(original, images):
    result = copy.deepcopy(original)
    result['name'] = 'stationd-v1-icecast'
    result['services'] = {name: result['services'][name] for name in ROLES}
    binds = {'/etc/stationd': './config/icecast',
             '/etc/stationd/router.toml': './config/icecast/router.toml',
             '/data/geoip': './data/geoip'}
    used_volumes = set()
    for name, service in result['services'].items():
        service.pop('build', None)
        service.pop('profiles', None)
        service['image'] = images['admin' if name == 'router' else name]
        service['pull_policy'] = 'never'
        service['volumes'] = [v for v in service.get('volumes', []) if v['target'] != '/central-backups']
        for volume in service['volumes']:
            if volume['type'] == 'bind':
                if volume['target'] not in binds:
                    raise ValueError('unexpected central bind: ' + volume['target'])
                volume['source'] = binds[volume['target']]
            else:
                used_volumes.add(volume['source'])
        if name == 'icecast':
            service['ports'] = ['${ICECAST_BIND:-127.0.0.1}:${ICECAST_PORT:-18000}:8000']
        elif name == 'admin':
            service['ports'] = ['127.0.0.1:${STATION_ADMIN_PORT:-50061}:50061']
    result['volumes'] = {name: {} for name in used_volumes}
    result['networks'] = {name: {k: v for k, v in definition.items() if k != 'name'}
                          for name, definition in result.get('networks', {}).items()}
    # Actual secret mounts are derived from the target configuration by install.py.
    used_secrets = {s['source'] for service in result['services'].values() for s in service.get('secrets', [])}
    result['secrets'] = {name: {'file': './.local/icecast/secrets/' + name} for name in used_secrets}
    return result


def sha(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--allow-dirty', action='store_true')
    parser.add_argument('--skip-build', action='store_true', help='package already built dev images')
    parser.add_argument('--vm', default=os.environ.get('VM', ''))
    args = parser.parse_args()
    dirty = bool(run(['git', '-C', str(ROOT), 'status', '--porcelain']))
    if dirty and not args.allow_dirty:
        parser.error('arbre Git modifié : utiliser --allow-dirty explicitement')
    if args.vm and not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_.@:-]*', args.vm):
        parser.error('cible SSH invalide')
    revision = run(['git', '-C', str(ROOT), 'rev-parse', '--short=8', 'HEAD'])
    version = tomllib.loads((ROOT / 'crates/stationd-admin/Cargo.toml').read_text())['package']['version']
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    tag = f'{version}-{revision}' + ('-dirty' if dirty else '') + '-' + stamp
    name = 'stationd-icecast-' + tag
    dist = ROOT / 'dist'
    dist.mkdir(exist_ok=True)
    dc = ['docker', 'compose', '-f', str(ROOT / 'docker/icecast/compose.yaml')]
    if not args.skip_build:
        subprocess.run([*dc, '--profile', 'monitor', 'build', 'icecast', 'admin', 'monitor'], check=True)
    images = {}
    identifiers = {}
    for role in ('icecast', 'admin', 'monitor'):
        repository = 'stationd-v1-' + ('icecast' if role == 'icecast' else role)
        image = f'{repository}:{tag}'
        subprocess.run(['docker', 'image', 'tag', repository + ':dev', image], check=True)
        images[role] = image
        identifiers[image] = run(['docker', 'image', 'inspect', image, '--format', '{{.Id}}'])
    original = json.loads(run([*dc, '--profile', '*', 'config', '--format', 'json']))
    with tempfile.TemporaryDirectory(prefix='icecast-package-', dir=dist) as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        config = stage / 'config/icecast'
        config.mkdir(parents=True)
        for filename in ('icecast.toml', 'admin.toml', 'router.toml', 'monitor.toml'):
            source = ROOT / 'config/icecast' / filename
            if source.is_symlink():
                raise ValueError('configuration symlink refused')
            shutil.copyfile(source, config / filename)
        monitor = tomllib.loads((config / 'monitor.toml').read_text())
        monitor['disk_paths'] = [p for p in monitor['disk_paths'] if p != '/central-backups']
        (config / 'monitor.toml').write_text('\n'.join(f'{k} = {json.dumps(v, ensure_ascii=False)}' for k, v in monitor.items()) + '\n')
        icecast = tomllib.loads((config / 'icecast.toml').read_text())
        geo = stage / 'data/geoip'
        geo.mkdir(parents=True)
        if icecast.get('geoip', {}).get('enabled'):
            filename = icecast['geoip']['database_file']
            if Path(filename).name != filename or filename.startswith('.'):
                raise ValueError('invalid GeoLite filename')
            source = ROOT / 'data/geoip' / filename
            if source.is_symlink() or not source.is_file():
                raise ValueError('configured GeoLite database missing or symlink')
            shutil.copyfile(source, geo / filename)
        (stage / 'compose.yaml').write_text(json.dumps(runtime_compose(original, images), indent=2))
        for filename in ('install.py', 'install.sh', 'compose', 'ctl'):
            shutil.copyfile(ROOT / 'docker/icecast/deploy' / filename, stage / filename)
            (stage / filename).chmod(0o755 if filename != 'install.py' else 0o644)
        shutil.copyfile(ROOT / 'Doc/v1-icecast-deploy.md', stage / 'README.md')
        (stage / 'VERSION').write_text(tag + '\n')
        (stage / 'manifest.json').write_text(json.dumps({'tag': tag, 'git_revision': revision, 'dirty': dirty, 'images': identifiers}, indent=2))
        print('== export des images centrales', flush=True)
        with gzip.open(stage / 'images.tar.gz', 'wb', compresslevel=1) as output:
            process = subprocess.Popen(['docker', 'image', 'save', *images.values()], stdout=subprocess.PIPE)
            try:
                shutil.copyfileobj(process.stdout, output)
            finally:
                process.stdout.close()
            if process.wait() != 0:
                raise RuntimeError('docker image save failed')
        files = sorted(p for p in stage.rglob('*') if p.is_file())
        (stage / 'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(stage).as_posix()}\n' for p in files))
        destination = dist / (name + '.tar')
        partial = dist / (name + '.partial')
        try:
            with tarfile.open(partial, 'w') as archive:
                archive.add(stage, arcname=name)
            partial.replace(destination)
        finally:
            partial.unlink(missing_ok=True)
    print('Paquet prêt : ' + str(destination), flush=True)
    if args.vm:
        subprocess.run(['bash', str(ROOT / 'docker/deploy.sh'), str(destination), args.vm], check=True)
    else:
        print(f'Livraison : bash docker/deploy.sh {destination} <cible SSH>')


if __name__ == '__main__':
    main()
