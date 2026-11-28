"""Owned loopback byte relay with an explicit fail-closed partition switch.

Never logs or stores packet contents (which may contain database credentials).
It cannot choose a model/tool target; the host fixes its one upstream socket.
"""
import select
import socket
import socketserver
import threading
import time


class OwnedTcpProxy:
    def __init__(self, host, port):
        if host not in {'127.0.0.1','::1'} or not 1<=port<=65535:
            raise ValueError('owned proxy requires a fixed loopback upstream')
        self.upstream=(host,port);self.lock=threading.Lock();self.enabled=True;self.links=set()
        self.stats={'accepted':0,'refused':0,'closed_on_isolation':0,'client_bytes':0,'server_bytes':0,'switches':[]}
        owner=self
        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                left=self.request;right=None;pair=None
                with owner.lock:
                    owner.stats['accepted']+=1
                    if not owner.enabled:owner.stats['refused']+=1;return
                try:
                    right=socket.create_connection(owner.upstream,timeout=3);left.setblocking(True);right.setblocking(True)
                    pair=(left,right)
                    with owner.lock:
                        if not owner.enabled:owner.stats['refused']+=1;return
                        owner.links.add(pair)
                    while True:
                        with owner.lock:
                            if not owner.enabled:return
                        readable,_,_=select.select([left,right],[],[],.05)
                        for source in readable:
                            data=source.recv(65536)
                            if not data:return
                            target=right if source is left else left;target.sendall(data)
                            with owner.lock:owner.stats['client_bytes' if source is left else 'server_bytes']+=len(data)
                except (OSError,ValueError):
                    return
                finally:
                    with owner.lock:
                        if pair is not None:owner.links.discard(pair)
                    for stream in [left,right]:
                        if stream is not None:
                            try:stream.close()
                            except OSError:pass
        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address=False
            daemon_threads=True
        self.server=Server(('127.0.0.1',0),Handler);self.port=self.server.server_address[1]
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
    def set_enabled(self, enabled):
        with self.lock:
            self.enabled=bool(enabled);links=list(self.links) if not enabled else []
            self.stats['switches'].append({'enabled':bool(enabled),'monotonic_ns':time.monotonic_ns(),'unix_ns':time.time_ns()})
            self.stats['closed_on_isolation']+=len(links)
        for pair in links:
            for stream in pair:
                try:stream.shutdown(socket.SHUT_RDWR)
                except OSError:pass
                try:stream.close()
                except OSError:pass
    def snapshot(self):
        with self.lock:return dict(self.stats,active_connections=len(self.links),enabled=self.enabled,switches=list(self.stats['switches']))
    def close(self):
        self.set_enabled(False);self.server.shutdown();self.server.server_close();self.thread.join(timeout=5)
