#!/usr/bin/env python3
"""Container-local Icecast lifecycle and StationDeck application handshake."""
import base64
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.request
import xml.etree.ElementTree as ET
sys.path.insert(0, str(Path(__file__).parent))
from icecast_routing_proxy import RoutingProxy, add_local_proxy_trust

BASE = Path('/etc/stationd/icecast.toml')
EFFECTIVE = Path('/managed/icecast.toml')
XML = Path('/tmp/icecast.xml')
SOCKET = os.environ.get('STATIONDECK_ICECAST_SOCKET', '/tmp/stationdeck-icecast.sock')
MAX_BYTES = 128 * 1024 + 8192
FIELDS = {'location': str, 'hostname': str, 'admin_email': str, 'max_clients': int}
STOP = False


def atomic(path, data, mode=0o600):
    fd, name = tempfile.mkstemp(prefix='.next-', dir=path.parent)
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def candidate_text(bundle):
    if bundle.get('authority') != 'stationdeck-dev' or bundle.get('target') != 'icecastd-1':
        raise ValueError('Unexpected configuration identity')
    revision = bundle.get('revision')
    if type(revision) is not int or revision <= 0:
        raise ValueError('Invalid revision')
    docs = bundle.get('documents', [])
    if len(docs) != 1:
        raise ValueError('Exactly one Icecast document is required')
    doc = docs[0]
    if (doc.get('plugin'), doc.get('instance'), doc.get('version')) != ('icecast.settings', 'default', 1):
        raise ValueError('Unsupported Icecast document')
    values = doc.get('values', {})
    if set(values) != set(FIELDS):
        raise ValueError('Unsupported Icecast fields')
    for key, kind in FIELDS.items():
        value = values[key]
        if type(value) is not kind:
            raise ValueError('Invalid Icecast field type')
        if kind is str and (not value.strip() or len(value.encode()) > 1024 or any(ord(c) < 32 or ord(c) == 127 for c in value)):
            raise ValueError('Invalid Icecast identity field')
    if not 1 <= values['max_clients'] <= 32768:
        raise ValueError('Invalid client limit')
    # Edit only four approved root scalars. Preserve mounts, secret references,
    # port, proxy policy, GeoIP and all other local bootstrap settings.
    baseline = BASE.read_text()
    before = tomllib.loads(baseline)
    text = baseline
    root = text.split('\n[', 1)[0]
    for key, value in values.items():
        pattern = rf'(?m)^{key}\s*=.*$'
        if len(re.findall(pattern, root)) != 1:
            raise ValueError('Expected a single root scalar in the local bootstrap')
        root = re.sub(pattern, lambda _: key + ' = ' + json.dumps(value, ensure_ascii=False), root)
    text = root + text[len(baseline.split('\n[', 1)[0]):]
    expected = dict(before)
    expected.update(values)
    if tomllib.loads(text) != expected:
        raise ValueError('Bootstrap preservation check failed')
    return text.encode()


def render(text):
    path = Path('/tmp/stationdeck-candidate.toml')
    output = Path('/tmp/stationdeck-candidate.xml')
    atomic(path, text)
    try:
        subprocess.run(['station-icecast-config', 'render', str(path), '/run/secrets', str(output)],
                       check=True, timeout=2, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return output.read_bytes()
    finally:
        path.unlink(missing_ok=True)
        output.unlink(missing_ok=True)


def healthy(process, text):
    if process.poll() is not None:
        return False
    cfg = tomllib.loads(text.decode())
    password = (Path('/run/secrets') / cfg['admin_password_file']).read_text().rstrip('\r\n')
    credential = base64.b64encode((cfg['admin_user'] + ':' + password).encode()).decode()
    req = urllib.request.Request('http://127.0.0.1:8000/admin/stats',
                                 headers={'Authorization': 'Basic ' + credential})
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(req, timeout=0.4) as response:
            stats = ET.fromstring(response.read(2 * 1024 * 1024))
        return all(stats.findtext(key) == cfg[field] for key, field in
                   [('host', 'hostname'), ('location', 'location'), ('admin', 'admin_email')])
    except (OSError, ValueError, ET.ParseError):
        return False


def start(xml, text):
    atomic(XML, add_local_proxy_trust(xml))
    child = subprocess.Popen(['icecast2', '-c', str(XML)], stdin=subprocess.DEVNULL)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline and not STOP:
        if healthy(child, text):
            return child
        if child.poll() is not None:
            break
        time.sleep(0.1)
    stop(child)
    raise RuntimeError('Icecast startup verification failed')


def stop(child):
    if child is not None and child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=1.5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=1)


def forward(path):
    data = Path(path).read_bytes()
    if len(data) > MAX_BYTES:
        raise ValueError('Candidate exceeds the size limit')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(13)
        client.connect(SOCKET)
        client.sendall(data)
        client.shutdown(socket.SHUT_WR)
        if client.recv(16) != b'applied\n':
            raise RuntimeError('Icecast application was not confirmed')


def shutdown(_signum, _frame):
    global STOP
    STOP = True


def serve():
    os.umask(0o077)
    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    # Only a publication or the agent's validated cache may start Icecast.
    # The effective TOML is an output, never a second configuration authority.
    active = None
    xml = None
    child = None
    agent = None
    announcer = None
    proxy = RoutingProxy(('0.0.0.0', 8001), ('127.0.0.1', 8000))
    proxy.start()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
        Path(SOCKET).unlink(missing_ok=True)
        server.bind(SOCKET)
        os.chmod(SOCKET, 0o666 if os.environ.get('STATIONDECK_ICECAST_SOCKET') else 0o600)
        server.listen(1)
        server.settimeout(0.25)
        try:
            print('Waiting for StationDeck configuration or a validated agent cache.', flush=True)
            agent = subprocess.Popen(['stationdeck-control', 'agent'])
            while not STOP:
                if (not proxy.thread.is_alive() or (child is not None and child.poll() is not None)
                        or agent.poll() is not None
                        or (announcer is not None and announcer.poll() is not None)):
                    raise RuntimeError('Managed child stopped; restarting the container is required')
                try:
                    client, _ = server.accept()
                except TimeoutError:
                    continue
                with client:
                    client.settimeout(1)
                    try:
                        payload = bytearray()
                        while True:
                            chunk = client.recv(8192)
                            if not chunk:
                                break
                            payload.extend(chunk)
                            if len(payload) > MAX_BYTES:
                                raise ValueError('Candidate exceeds the size limit')
                        bundle = json.loads(payload)
                        if set(bundle) == {'route'}:
                            if child is None or child.poll() is not None:
                                raise RuntimeError('Icecast is not running')
                            proxy.apply(bundle['route'])
                            client.sendall(b'applied\n')
                            continue
                        text = candidate_text(bundle)
                        next_xml = render(text)  # Validate before stopping the running process.
                        if child is None or text != active or not healthy(child, active):
                            previous, previous_xml = active, xml
                            proxy.configure(tomllib.loads(text.decode()))
                            stop(child)
                            try:
                                child = start(next_xml, text)
                                atomic(EFFECTIVE, text, 0o644)
                            except Exception:
                                stop(child)
                                child = None
                                if previous is not None:
                                    proxy.configure(tomllib.loads(previous.decode()))
                                    child = start(previous_xml, previous)
                                    atomic(EFFECTIVE, previous, 0o644)
                                raise
                            active, xml = text, next_xml
                            if announcer is None and os.environ.get('STATIONDECK_SERVICE_DESCRIPTOR'):
                                announcer = subprocess.Popen(['stationdeck-discovery'])
                            print('Confirmed Icecast revision {}.'.format(bundle['revision']), flush=True)
                        try:
                            client.sendall(b'applied\n')
                        except OSError:
                            pass  # Agent reconciles an unknown outcome on its next pull.
                    except Exception:
                        print('Icecast configuration rejected or application failed.', file=sys.stderr, flush=True)
                        if child is not None and child.poll() is not None:
                            raise RuntimeError('Icecast rollback failed')
                        try:
                            client.sendall(b'failed\n')
                        except OSError:
                            pass
        finally:
            proxy.close()
            stop(announcer)
            stop(agent)
            stop(child)
            Path(SOCKET).unlink(missing_ok=True)


if __name__ == '__main__':
    try:
        if len(sys.argv) == 3 and sys.argv[1] == 'apply':
            forward(sys.argv[2])
        elif len(sys.argv) == 1:
            serve()
        else:
            raise ValueError('Invalid invocation')
    except Exception:
        print('StationDeck Icecast adapter failed; inspect container health.', file=sys.stderr)
        sys.exit(1)
