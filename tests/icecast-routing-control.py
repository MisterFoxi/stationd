"""Listener entry routing: real TCP forwarding and request-boundary checks."""
from pathlib import Path
import socket
import socketserver
import sys
import threading
import unittest
import xml.etree.ElementTree as ET
sys.path.insert(0, str(Path(__file__).parents[1] / 'docker/stationdeck'))
from icecast_routing_proxy import RoutingProxy, add_local_proxy_trust

MARKER = 'a' * 32
CFG = {'mounts': [{'name':'/a'}, {'name':'/b'}, {'name':'/c'}], 'trusted_proxies':['192.0.2.10']}

def change(enabled=True, origin='/a', target='/b', marker=MARKER):
    return {'mount':origin,'destination':target,'enabled':enabled,'marker':marker}

class ProxyTest(unittest.TestCase):
    def setUp(self):
        self.proxy = RoutingProxy(('127.0.0.1',0), ('127.0.0.1',1))
        self.proxy.configure(CFG)
    def tearDown(self):
        self.proxy.server.server_close()
    def request(self, path='/a?auth=preserved', method='GET', extra=''):
        return (f'{method} {path} HTTP/1.1\r\nHost: radio.local\r\nUser-Agent: original\r\nAuthorization: Bearer unchanged\r\n'+extra+'\r\n').encode()
    def test_empty_route_applies_to_future_requests_and_restore_removes_it(self):
        self.proxy.apply(change())
        actual=self.proxy.rewrite(self.request(extra='X-Forwarded-For: 8.8.8.8\r\n'), '198.51.100.9').decode()
        self.assertTrue(actual.startswith('GET /b?auth=preserved HTTP/1.1'))
        self.assertIn('Authorization: Bearer unchanged',actual)
        self.assertIn('User-Agent: StationDeckRoute/'+MARKER,actual)
        self.assertIn('X-Forwarded-For: 198.51.100.9',actual)
        self.assertNotIn('8.8.8.8',actual)
        native=self.proxy.rewrite(self.request('/b'), '198.51.100.9').decode()
        self.assertIn('User-Agent: original',native)
        self.proxy.apply(change(False))
        self.assertTrue(self.proxy.rewrite(self.request(), '198.51.100.9').startswith(b'GET /a?auth=preserved '))
    def test_source_uploads_and_head_are_preserved_and_only_trusted_peers_forward_ips(self):
        self.proxy.apply(change())
        upload=self.request('/a','PUT','Expect: 100-continue\r\n')
        self.assertEqual(self.proxy.rewrite(upload,'198.51.100.9'),upload)
        head=self.proxy.rewrite(self.request('/a','HEAD'),'198.51.100.9')
        self.assertTrue(head.startswith(b'HEAD /a '))
        trusted=self.proxy.rewrite(self.request(extra='X-Forwarded-For: 198.51.100.44\r\n'),'192.0.2.10')
        self.assertIn(b'X-Forwarded-For: 198.51.100.44',trusted)
    def test_chains_cycles_unknown_targets_and_foreign_ownership_are_rejected(self):
        self.proxy.apply(change())
        for request in [change(origin='/b',target='/a'),change(origin='/c',target='/a'),change(target='/unknown'),change(False,marker='b'*32)]:
            with self.assertRaises(ValueError): self.proxy.apply(request)
    def test_ambiguous_headers_cannot_smuggle_a_forwarded_identity(self):
        for extra in ['X-Forwarded-For : 8.8.8.8\r\n', ' folded: 8.8.8.8\r\n', 'bad-header\r\n']:
            with self.assertRaises(ValueError): self.proxy.rewrite(self.request(extra=extra),'198.51.100.9')
    def test_loopback_forwarding_trust_does_not_replace_existing_proxy_policy(self):
        xml=b'<icecast><listen-socket><port>8000</port><trusted-proxy>#existing</trusted-proxy></listen-socket></icecast>'
        tree=ET.fromstring(add_local_proxy_trust(xml))
        self.assertEqual([n.text for n in tree.findall('listen-socket/trusted-proxy')],['#existing','#stationdeck-listening-proxy'])
        self.assertEqual(tree.find("listen-socket[@id='stationdeck-listening-proxy']/client-address").text,'127.0.0.1')
    def test_tcp_forwarding_keeps_an_existing_stream_open_during_route_change(self):
        released=threading.Event()
        opened=threading.Event()
        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                data=b''
                while b'\r\n\r\n' not in data: data+=self.request.recv(4096)
                path=data.split(b' ')[1]
                if path==b'/a':
                    self.request.sendall(b'HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nORIGIN')
                    opened.set();released.wait(4)
                    self.request.sendall(b'-STILL-PLAYING')
                else:
                    self.request.sendall(b'HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nTARGET')
        class Server(socketserver.ThreadingTCPServer):
            daemon_threads=True
        server=Server(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        self.proxy.upstream=server.server_address
        self.proxy.start()
        try:
            with socket.create_connection(self.proxy.server.server_address,timeout=3) as old:
                old.sendall(self.request('/a'))
                first=old.recv(4096);self.assertIn(b'ORIGIN',first);self.assertTrue(opened.wait(1))
                self.proxy.apply(change())
                with socket.create_connection(self.proxy.server.server_address,timeout=3) as new:
                    new.sendall(self.request('/a'))
                    reply=b''
                    while True:
                        part=new.recv(4096)
                        if not part: break
                        reply+=part
                    self.assertIn(b'TARGET',reply)
                released.set();self.assertIn(b'STILL-PLAYING',old.recv(4096))
        finally:
            released.set();self.proxy.close();server.shutdown();server.server_close()

if __name__=='__main__': unittest.main()