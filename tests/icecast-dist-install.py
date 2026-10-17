from pathlib import Path
import json
import subprocess
import tarfile
import tempfile
import uuid
import argparse

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description='Validate the real deployment bundle without starting services')
parser.add_argument('archive', type=Path)
archive = parser.parse_args().archive.resolve()
dc = ['docker', 'compose', '-f', str(root / 'docker/icecast/compose.yaml')]


def live():
    ids = subprocess.check_output([*dc, 'ps', '-q'], text=True).split()
    if not ids:
        return {}
    return {c['Name']: (c['Id'], c['State']['StartedAt']) for c in json.loads(subprocess.check_output(['docker', 'inspect', *ids], text=True))}


before = live()
with tempfile.TemporaryDirectory(prefix='stationd-icecast-dist-test-') as folder:
    temp = Path(folder)
    with tarfile.open(archive) as source:
        names = source.getnames()
        assert not any('/.local/' in name or 'backup.py' in name or '/recovery' in name for name in names)
        source.extractall(temp, filter='data')
    bundle = temp / archive.stem
    subprocess.run([str(bundle / 'install.sh'), '--help'], check=True, stdout=subprocess.DEVNULL)
    node = temp / 'node'
    project = 'icecast-dist-test-' + uuid.uuid4().hex[:10]
    command = ['python3', str(bundle / 'install.py'), '--dir', str(node), '--project', project, '--no-start']
    subprocess.run(command, check=True)
    subprocess.run([str(node / 'compose'), 'config', '--quiet'], check=True)
    secret = node / '.local/icecast/secrets/admin-api-token'
    previous_secret = secret.read_bytes()
    config = node / 'config/icecast/icecast.toml'
    modified = config.read_text() + '\n# Site customization retained\n[[mounts]]\nname="/dist-extra.mp3"\npassword_file="dist-extra-password"\naudience=false\nhidden=true\n'
    config.write_text(modified)
    env = b'ICECAST_BIND=127.0.0.1\nICECAST_PORT=18443\nSITE_LOCAL=retained\n'
    (node / '.env').write_bytes(env)
    subprocess.run(command, check=True)
    assert secret.read_bytes() == previous_secret
    assert secret.parent.stat().st_mode & 0o777 == 0o700
    assert config.read_text() == modified
    assert (node / '.env').read_bytes() == env
    deployment = json.loads((node / 'compose.yaml').read_text())
    assert 'dist-extra-password' in deployment['services']['icecast']['secrets']
    assert (secret.parent / 'dist-extra-password').is_file()
    assert set(deployment['services']) == {'icecast', 'admin', 'router', 'monitor'}
    assert '/central-backups' not in (node / 'config/icecast/monitor.toml').read_text()
    assert not subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'label=com.docker.compose.project=' + project], text=True).strip()
assert live() == before
print('PASS: real bundle verified, images loaded, TOML rendered, fresh install and repeat install preserve site configuration/secrets/env; dynamic extra mount provisioned; no service started; live containers unchanged')
