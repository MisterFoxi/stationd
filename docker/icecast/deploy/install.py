#!/usr/bin/env python3
"""Install the central Icecast V1 runtime without replacing node configuration or state."""
import argparse
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tomllib


def verify_bundle(root):
    names = set()
    for line in (root / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        path = Path(name)
        if path.is_absolute() or '..' in path.parts or name in names or not re.fullmatch('[0-9a-f]{64}', digest):
            raise ValueError('invalid checksum inventory')
        names.add(name)
        file = root / path
        if file.is_symlink() or not file.is_file():
            raise ValueError('invalid bundle member: ' + name)
        with file.open('rb') as source:
            if hashlib.file_digest(source, 'sha256').hexdigest() != digest:
                raise ValueError('bundle checksum mismatch: ' + name)
    actual = {p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file() and p.name != 'SHA256SUMS'}
    if actual != names:
        raise ValueError('bundle inventory mismatch')


def secret_plan(deployment, icecast, router):
    result = json.loads(json.dumps(deployment))
    names = {icecast['admin_password_file'], *(m['password_file'] for m in icecast['mounts'])}
    if router['output_password_file'] in {'admin-api-token', 'router-api-token'}:
        raise ValueError('API tokens cannot be used as audio passwords')
    if names & {'admin-api-token', 'router-api-token'}:
        raise ValueError('API tokens cannot be used as audio passwords')
    result['services']['icecastd']['secrets'] = sorted(names)
    result['services']['admin']['secrets'] = [icecast['admin_password_file'], 'admin-api-token', 'router-api-token']
    result['services']['router']['secrets'] = [router['output_password_file'], 'router-api-token']
    result['services']['monitor']['secrets'] = ['admin-api-token']
    names.update((router['output_password_file'], 'admin-api-token', 'router-api-token'))
    if not all(re.fullmatch('[A-Za-z0-9_-]+', name) for name in names):
        raise ValueError('invalid secret name')
    result['secrets'] = {name: {'file': './.local/icecast/secrets/' + name} for name in sorted(names)}
    return result


def invoke(args):
    subprocess.run(args, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dir', type=Path, default=Path('/opt/stationd-icecast'))
    parser.add_argument('--project')
    parser.add_argument('--bind', type=ipaddress.ip_address)
    parser.add_argument('--port', type=int)
    parser.add_argument('--no-start', action='store_true', help='prepare and validate without starting services')
    args = parser.parse_args()
    if not args.dir.is_absolute() or args.dir.resolve() == Path('/'):
        parser.error('--dir must be an absolute application directory')
    if args.port is not None and not 1 <= args.port <= 65535:
        parser.error('--port must be 1..65535')
    here = Path(__file__).resolve().parent
    verify_bundle(here)
    manifest = json.loads((here / 'manifest.json').read_text())
    node = args.dir.resolve()
    if node == here or here.is_relative_to(node):
        parser.error('installation directory must be separate from extracted bundle')
    marker = node / 'INSTALLATION'
    if node.exists() and any(node.iterdir()) and (not marker.is_file() or marker.read_text() != 'stationd-v1-icecast\n'):
        parser.error('non-empty directory is not a managed Icecast V1 installation')
    invoke(['docker', 'compose', 'version'])
    invoke(['docker', 'info', '--format', '{{.OSType}}'])
    node.mkdir(parents=True, exist_ok=True, mode=0o700)
    marker.write_text('stationd-v1-icecast\n')
    with (node / '.install.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        project_file = node / 'PROJECT'
        project = project_file.read_text().strip() if project_file.exists() else (args.project or 'stationd-v1-icecast')
        if not re.fullmatch('[a-z0-9][a-z0-9_-]*', project) or (args.project and project != args.project):
            parser.error('invalid project or change of existing volume identity')
        # Refuse to replace a different installation that uses the same Docker project.
        containers = subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'label=com.docker.compose.project=' + project], text=True).split()
        if containers:
            existing = json.loads(subprocess.check_output(['docker', 'inspect', *containers], text=True))
            for container in existing:
                path = container['Config']['Labels'].get('com.docker.compose.project.working_dir', '')
                if Path(path).resolve() != node:
                    raise ValueError('Docker project already belongs to another installation: ' + project)
        project_file.write_text(project + '\n')
        invoke(['docker', 'image', 'load', '-i', str(here / 'images.tar.gz')])
        for image, expected in manifest['images'].items():
            actual = subprocess.check_output(['docker', 'image', 'inspect', image, '--format', '{{.Id}}'], text=True).strip()
            if actual != expected:
                raise ValueError('loaded image mismatch: ' + image)
        for source in (here / 'config/icecast').iterdir():
            target = node / 'config/icecast' / source.name
            target.parent.mkdir(parents=True, exist_ok=True)
            if not target.exists():
                shutil.copyfile(source, target)
        geo = node / 'data/geoip'
        geo.mkdir(parents=True, exist_ok=True)
        for source in (here / 'data/geoip').iterdir():
            if not (geo / source.name).exists():
                shutil.copyfile(source, geo / source.name)
        icecast = tomllib.loads((node / 'config/icecast/icecast.toml').read_text())
        router = tomllib.loads((node / 'config/icecast/router.toml').read_text())
        deployment = secret_plan(json.loads((here / 'compose.yaml').read_text()), icecast, router)
        secret_dir = node / '.local/icecast/secrets'
        if secret_dir.is_symlink():
            raise ValueError('secret directory cannot be a symlink')
        secret_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        secret_dir.chmod(0o700)
        for name in deployment['secrets']:
            file = secret_dir / name
            if file.is_symlink():
                raise ValueError('secret file cannot be a symlink')
            if not file.exists():
                with file.open('x') as output:
                    output.write(secrets.token_hex(24) + '\n')
            if not file.read_text().strip():
                raise ValueError('empty secret: ' + name)
            file.chmod(0o644)  # private parent; individual read-only Docker mounts
        # Validate configuration and secrets before touching the active Compose file.
        command = ['docker', 'run', '--rm', '--network', 'none', '--read-only', '--cap-drop', 'ALL',
                   '--security-opt', 'no-new-privileges:true', '--tmpfs', '/tmp:mode=1777',
                   '--mount', f'type=bind,src={node / "config/icecast"},dst=/etc/stationd,readonly',
                   '--mount', f'type=bind,src={geo},dst=/data/geoip,readonly']
        for name in deployment['secrets']:
            command += ['--mount', f'type=bind,src={secret_dir / name},dst=/run/secrets/{name},readonly']
        command += ['--entrypoint', 'station-icecast-config', deployment['services']['icecastd']['image'],
                    'render', '/etc/stationd/icecast.toml', '/run/secrets', '/tmp/icecast.xml']
        invoke(command)
        compose = node / 'compose.yaml'
        env_file = node / '.env'
        previous = compose.read_bytes() if compose.exists() else None
        if previous is not None:
            old_services = json.loads(subprocess.check_output(
                ['docker', 'compose', '-p', project, '-f', str(compose), 'config', '--format', 'json'], text=True))['services']
            if 'icecast' in old_services:
                old_container = subprocess.check_output(
                    ['docker', 'compose', '-p', project, '-f', str(compose), 'ps', '-q', 'icecast'], text=True).strip()
                if old_container:
                    raise ValueError('Stop the previous V1 icecast service before upgrading to icecastd; see the migration guide')
        previous_env = env_file.read_bytes() if env_file.exists() else None
        env = env_file.read_text() if env_file.exists() else 'ICECAST_BIND=127.0.0.1\nICECAST_PORT=18000\n'
        for key, value in (('ICECAST_BIND', args.bind), ('ICECAST_PORT', args.port)):
            if value is not None:
                value = str(value)
                if key == 'ICECAST_BIND' and ':' in value:
                    value = '[' + value + ']'
                env = re.sub(r'^' + key + r'=.*\n?', '', env, flags=re.MULTILINE)
                env += '\n' + key + '=' + value + '\n'
        candidate = node / '.compose.next.json'
        candidate.write_text(json.dumps(deployment, indent=2))
        env_file.write_text(env)
        dc = ['docker', 'compose', '-p', project, '--env-file', str(env_file)]
        try:
            invoke([*dc, '-f', str(candidate), 'config', '--quiet'])
            candidate.replace(compose)
            for name in ('compose', 'ctl'):
                shutil.copyfile(here / name, node / name)
                (node / name).chmod(0o755)
            if not args.no_start:
                invoke([*dc, '-f', str(compose), 'up', '-d', '--no-build', '--pull', 'never', '--wait', 'icecastd', 'admin'])
                invoke([*dc, '-f', str(compose), 'up', '-d', '--no-build', '--pull', 'never', '--no-deps', 'router', 'monitor'])
        except BaseException:
            candidate.unlink(missing_ok=True)
            if previous_env is not None:
                env_file.write_bytes(previous_env)
            elif previous is not None:
                env_file.unlink(missing_ok=True)
            if previous is not None:
                compose.write_bytes(previous)
                if not args.no_start:
                    subprocess.run([*dc, '-f', str(compose), 'up', '-d', '--no-build', '--pull', 'never'])
            # On a first installation, retain the validated Compose and .env so
            # the operator can inspect a partial startup and retry with ./compose.
            raise
        (node / 'VERSION').write_text(manifest['tag'] + '\n')
        print('Icecast V1 installé : ' + str(node))
        print('Configurations, secrets et volumes existants conservés.')
        print('Adresse/port : ' + str(env_file) + ' ; client gRPC : ' + str(node / 'ctl'))
        if args.no_start:
            print('Préparation validée ; aucun service démarré.')


if __name__ == '__main__':
    main()
