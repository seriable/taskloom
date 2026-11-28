"""Actual loopback HTTP contract and safety checks for the Python SDK."""
import http.server
import json
from pathlib import Path
import sys
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'sdks/python'))
from taskloom_client import Client, TaskloomError


class Server:
    def __init__(self,status=200,body=b'{"ok":true}',redirect=None,declared_length=None):
        self.requests=[]
        owner=self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_GET(self): self.do_POST()
            def do_POST(self):
                data=self.rfile.read(int(self.headers.get('Content-Length','0')))
                owner.requests.append((self.command,self.path,dict(self.headers),json.loads(data) if data else None))
                self.send_response(status)
                if redirect:self.send_header('Location',redirect)
                self.send_header('Content-Length',str(len(body) if declared_length is None else declared_length))
                self.end_headers()
                self.wfile.write(body)
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True)
    def __enter__(self):
        self.thread.start();self.url='http://127.0.0.1:'+str(self.server.server_port);return self
    def __exit__(self,*args):
        self.server.shutdown();self.server.server_close();self.thread.join()


class SDK(unittest.TestCase):
    def test_routes_and_exact_approval(self):
        with Server() as server:
            client=Client(server.url+'/tenants/t/projects/p/api','test-secret')
            client.run('one','hello')
            client.approve('one','ticket','alice','checked')
            client.session_turn('conversation','turn-1','next')
            client.events('one',200)
            client.inspect('session-'+'a'*64)
            self.assertEqual(server.requests[0][3],{'id':'one','prompt':'hello'})
            self.assertEqual(server.requests[1][1],'/tenants/t/projects/p/api/runs/one/approvals/ticket')
            self.assertEqual(server.requests[1][3],{'approved':True,'actor':'alice','note':'checked'})
            self.assertEqual(server.requests[2][3],{'turn_id':'turn-1','prompt':'next'})
            self.assertTrue(server.requests[3][1].endswith('/events?after=200'))
            self.assertTrue(server.requests[4][1].endswith('/runs/session-'+'a'*64))
            self.assertNotIn('test-secret',repr(client))

    def test_redirect_does_not_forward_or_retry(self):
        with Server() as destination:
            with Server(302,redirect=destination.url+'/capture') as origin:
                with self.assertRaises(TaskloomError) as error:
                    Client(origin.url+'/api','test-secret').resume('one')
                self.assertEqual(error.exception.status,302)
                self.assertEqual(len(origin.requests),1)
                self.assertEqual(len(destination.requests),0)

    def test_errors_bounds_and_non_json(self):
        for status,body,kind in [(409,b'private-server-data','conflict'),(200,b'x'*2048,'response_limit_outcome_unknown'),(200,b'bad','invalid_response_outcome_unknown'),(200,b'{"value":NaN}','invalid_response_outcome_unknown'),(200,b'{"value":1e400}','invalid_response_outcome_unknown')]:
            with Server(status,body) as server:
                with self.assertRaises(TaskloomError) as error:
                    Client(server.url+'/api','test-secret',max_response_bytes=1024).inspect('one')
                self.assertEqual(error.exception.kind,kind)
                self.assertNotIn('private-server-data',str(error.exception))
                self.assertEqual(len(server.requests),1)

    def test_ambient_proxy_never_receives_token_and_truncated_json_is_unknown(self):
        with Server() as proxy, Server() as host:
            with patch.dict('os.environ',{'http_proxy':proxy.url,'HTTP_PROXY':proxy.url,'no_proxy':'','NO_PROXY':''}):
                self.assertEqual(Client(host.url+'/api','test-secret').inspect('one'),{'ok':True})
            self.assertEqual(len(host.requests),1);self.assertEqual(proxy.requests,[])
        with Server(body=b'{"ok":true}',declared_length=100) as host:
            with self.assertRaises(TaskloomError) as error:Client(host.url+'/api','test-secret').inspect('one')
            self.assertEqual(error.exception.kind,'transport_failure_outcome_unknown')
            self.assertEqual(len(host.requests),1)

    def test_invalid_config_and_paths_do_not_dispatch(self):
        for base in ['http://example.com/api','https://a:b@example.com/api','https://example.com/api?token=x','https://example.com/api#x']:
            with self.assertRaises(ValueError):Client(base,'test')
        with Server() as server:
            client=Client(server.url+'/api','test-secret')
            for path in ['/../capture','//capture','/runs?secret=anything','/runs/../../capture']:
                with self.assertRaises(ValueError):client.request('POST',path,{})
            with self.assertRaises(ValueError):client.events('one',-1)
            self.assertEqual(server.requests,[])


if __name__=='__main__':unittest.main()
