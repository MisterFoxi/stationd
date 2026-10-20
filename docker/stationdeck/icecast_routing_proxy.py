"""Bounded TCP entry point for listener routes; source uploads pass through.

No Icecast restart/reload, media buffering or listener identity guessed by IP.
The public port is 8001; Icecast remains on the private port 8000.
"""
import ipaddress
import select
import socket
import socketserver
import threading
import urllib.parse
import xml.etree.ElementTree as ET


def add_local_proxy_trust(xml):
    tree = ET.fromstring(xml)
    real = next(n for n in tree.findall('listen-socket') if n.findtext('port') == '8000')
    ET.SubElement(real, 'trusted-proxy').text = '#stationdeck-listening-proxy'
    virtual = ET.SubElement(tree, 'listen-socket', {'id': 'stationdeck-listening-proxy', 'type': 'virtual'})
    ET.SubElement(virtual, 'client-address').text = '127.0.0.1'
    return ET.tostring(tree, encoding='utf-8', xml_declaration=True)


class RoutingProxy:
    def __init__(self, address, upstream):
        self.upstream = upstream
        self.lock = threading.Lock()
        self.routes = {}
        self.allowed = set()
        self.trusted = set()
        self.slots = threading.BoundedSemaphore(256)
        owner = self
        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address = True
            daemon_threads = True
            request_queue_size = 64
            def verify_request(self, request, client_address):
                return owner.slots.acquire(blocking=False)
            def process_request_thread(self, request, client_address):
                try:
                    super().process_request_thread(request, client_address)
                finally:
                    owner.slots.release()
            def handle_error(self, request, client_address):
                pass  # Never log request bytes, credentials or listener identities.
        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                owner.relay(self.request, self.client_address[0])
        self.server = Server(address, Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.2}, daemon=True)
    def start(self):
        self.thread.start()
    def close(self):
        self.server.shutdown()
        self.server.server_close()
    def configure(self, cfg):
        with self.lock:
            self.allowed = {m['name'] for m in cfg['mounts']}
            self.trusted = set(cfg.get('trusted_proxies', []))
            self.routes.clear()
    def apply(self, request):
        if set(request) != {'mount', 'destination', 'enabled', 'marker'} or type(request['enabled']) is not bool:
            raise ValueError('Invalid stream route request')
        origin, target, marker = request['mount'], request['destination'], request['marker']
        if not isinstance(marker, str) or len(marker) != 32 or any(c not in '0123456789abcdef' for c in marker):
            raise ValueError('Invalid connection marker')
        with self.lock:
            if origin not in self.allowed or target not in self.allowed or origin == target:
                raise ValueError('Unknown stream endpoint')
            if request['enabled']:
                if target in self.routes or any(value[0] == origin for value in self.routes.values()):
                    raise ValueError('Route chains are not supported')
                previous = self.routes.get(origin)
                if previous is not None and previous != (target, marker):
                    raise ValueError('Origin already routed')
                self.routes[origin] = (target, marker)
            else:
                previous = self.routes.get(origin)
                if previous is not None and previous != (target, marker):
                    raise ValueError('Route ownership changed')
                self.routes.pop(origin, None)
    def rewrite(self, header, peer):
        lines = header.decode('latin-1').split('\r\n')
        method, uri, version = lines[0].split(' ', 2)
        if method not in ('GET', 'HEAD'):
            return header  # Preserve SOURCE/PUT uploads, Expect and body framing.
        parsed = urllib.parse.urlsplit(uri)
        if parsed.scheme or parsed.netloc:
            raise ValueError('Origin-form HTTP requests required')
        fields = []
        token = "!#$%&'*+-.^_`|~"
        for line in lines[1:]:
            if not line:
                continue
            if ':' not in line:
                raise ValueError('Invalid HTTP header')
            name, value = line.split(':', 1)
            if not name or any(not (c.isascii() and (c.isalnum() or c in token)) for c in name):
                raise ValueError('Invalid HTTP header name')
            fields.append((name.lower(), value.strip(), line))
        with self.lock:
            route = self.routes.get(urllib.parse.unquote(parsed.path)) if method == 'GET' else None
            trusted = peer in self.trusted
        forwarded = peer
        supplied = next((v for k, v, _ in fields if k == 'x-forwarded-for'), '')
        if trusted and supplied:
            chain = [str(ipaddress.ip_address(value.strip())) for value in supplied.split(',')]
            if len(chain) > 16:
                raise ValueError('Forwarded chain limit')
            forwarded = ', '.join(chain + [peer])
        if route:
            target, marker = route
            uri = urllib.parse.quote(target, safe='/-._~') + ('?' + parsed.query if parsed.query else '')
        output = [method + ' ' + uri + ' ' + version]
        for key, value, original in fields:
            if key in ('connection', 'x-forwarded-for') or (route and key == 'user-agent'):
                continue
            output.append(original)
        if route:
            output.append('User-Agent: StationDeckRoute/' + marker)
        output.extend(['X-Forwarded-For: ' + forwarded, 'Connection: close', '', ''])
        return '\r\n'.join(output).encode('latin-1')
    def relay(self, client, peer):
        client.settimeout(15)
        data = bytearray()
        while b'\r\n\r\n' not in data:
            part = client.recv(4096)
            if not part:
                return
            data.extend(part)
            if len(data) > 65536:
                raise ValueError('HTTP header limit')
        header, tail = bytes(data).split(b'\r\n\r\n', 1)
        request = self.rewrite(header + b'\r\n\r\n', peer) + tail
        with socket.create_connection(self.upstream, timeout=3) as upstream:
            upstream.settimeout(30)
            upstream.sendall(request)
            client.settimeout(30)
            pair = {client: upstream, upstream: client}
            while True:
                ready, _, _ = select.select(list(pair), [], [], 30)
                if not ready:
                    return
                for source in ready:
                    block = source.recv(65536)
                    if not block:
                        return
                    pair[source].sendall(block)