"""Actual loopback sockets: isolation closes existing links and rejects new ones."""
import socket
import socketserver
import sys
from pathlib import Path
import threading
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'examples/operations'))
from owned_tcp_proxy import OwnedTcpProxy


class Proxy(unittest.TestCase):
    def test_existing_and_new_connections_fail_closed_then_new_connections_recover(self):
        class Echo(socketserver.BaseRequestHandler):
            def handle(self):
                while data:=self.request.recv(1024):self.request.sendall(data)
        class Server(socketserver.ThreadingTCPServer):daemon_threads=True
        upstream=Server(('127.0.0.1',0),Echo);thread=threading.Thread(target=upstream.serve_forever,daemon=True);thread.start()
        proxy=OwnedTcpProxy('127.0.0.1',upstream.server_address[1])
        try:
            client=socket.create_connection(('127.0.0.1',proxy.port),timeout=3);client.sendall(b'first');self.assertEqual(client.recv(5),b'first')
            proxy.set_enabled(False)
            try:self.assertEqual(client.recv(1),b'')
            except (ConnectionResetError,OSError):pass
            client.close()
            other=socket.create_connection(('127.0.0.1',proxy.port),timeout=3)
            try:other.sendall(b'blocked');self.assertEqual(other.recv(1),b'')
            except (ConnectionResetError,OSError):pass
            other.close();proxy.set_enabled(True)
            with socket.create_connection(('127.0.0.1',proxy.port),timeout=3) as recovered:
                recovered.sendall(b'last');self.assertEqual(recovered.recv(4),b'last')
            stats=proxy.snapshot();self.assertGreaterEqual(stats['closed_on_isolation'],1);self.assertGreaterEqual(stats['refused'],1)
            self.assertNotIn('packets',stats)
        finally:proxy.close();upstream.shutdown();upstream.server_close();thread.join(timeout=5)
    def test_nonloopback_target_is_refused(self):
        with self.assertRaises(ValueError):OwnedTcpProxy('example.com',5432)


if __name__=='__main__':unittest.main()
