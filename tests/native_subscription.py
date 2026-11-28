"""Offline transport admission controls; no remote API or model capability claims."""
import concurrent.futures
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import urllib.request
import urllib.error

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('subscription',ROOT/'examples/benchmarks/native_subscription.py')
module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


class SubscriptionControls(unittest.TestCase):
    def test_parallel_attempt_admission_is_bounded_and_durable(self):
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'requests.jsonl'; ledger=module.RequestLedger(path,3)
            with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
                attempts=list(pool.map(lambda _:ledger.reserve('hash'), range(20)))
            self.assertEqual(sorted(v for v in attempts if v is not None),[1,2,3])
            entries=[json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual([v['ordinal'] for v in entries],[1,2,3])
            self.assertTrue(all(v['event']=='reserved_before_dispatch' for v in entries))
            ledger.settle(1,status='http_error_usage_not_reported')
            ledger.settle(2,status='uncertain_after_reservation')
            self.assertIsNone(ledger.reserve('not refunded'))
            with self.assertRaises(FileExistsError): module.RequestLedger(path,3)

    def test_journal_failure_prevents_reservation(self):
        with tempfile.TemporaryDirectory() as folder:
            ledger=module.RequestLedger(Path(folder)/'requests.jsonl',2)
            with patch.object(ledger,'_append',side_effect=OSError('disk failure')):
                with self.assertRaises(OSError):ledger.reserve('hash')
            self.assertEqual(ledger.count,0)

    def test_unbounded_or_zero_allocation_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            for value in [0,-1,1001,True,1.5]:
                with self.assertRaises(ValueError):module.RequestLedger(Path(folder)/'journal',value)

    def test_metadata_cannot_describe_another_model_or_transport(self):
        with tempfile.TemporaryDirectory() as folder:
            provider={'base_url':'http://127.0.0.1:1234/v1','key_env':'LOCAL','type':'openai'}
            cfg={'model':'chosen','provider':provider}
            meta={'upstream_base_url':module.ALLOWED_BASE,'model':'chosen','local_provider':provider}
            path=Path(folder)/'metadata.json';path.write_text(json.dumps(meta))
            with patch.dict(os.environ,{module.METADATA_ENV:str(path)}):
                self.assertEqual(module.metadata_for(cfg),meta)
                with self.assertRaises(ValueError):module.metadata_for(dict(cfg,model='other'))
                with self.assertRaises(ValueError):module.metadata_for(dict(cfg,provider=dict(provider,base_url='http://127.0.0.1:1235/v1')))

    def test_redirects_do_not_forward_credentials(self):
        self.assertIsNone(module.NoRedirect().redirect_request(None,None,302,None,None,None))

    def test_http_auth_model_and_exhaustion_prevent_upstream_dispatch(self):
        # Explicit offline HTTP fixture; no remote inference or network route.
        with tempfile.TemporaryDirectory() as folder:
            directory=Path(folder);ledger=module.RequestLedger(directory/'ledger',1)
            server=module.http.server.ThreadingHTTPServer(('127.0.0.1',0),
                module.make_handler(ledger,directory,'chosen','private-fixture-key','local-token'))
            thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
            local=urllib.request.build_opener()
            observed=[]
            class Response:
                status=200
                def __enter__(self):return self
                def __exit__(self,*args):pass
                def read(self,*args):return b'{"model":"chosen","usage":{"prompt_tokens":1}}'
            class Upstream:
                def open(self,request,**kwargs):
                    observed.append(request);return Response()
            def post(model='chosen',token='local-token'):
                request=urllib.request.Request(f'http://127.0.0.1:{server.server_port}/v1/chat/completions',
                    data=json.dumps({'model':model,'messages':[]}).encode(),headers={'Authorization':'Bearer '+token})
                try:
                    with local.open(request,timeout=5) as response:return response.status
                except urllib.error.HTTPError as error:
                    with error:return error.code
            try:
                with patch.object(module.urllib.request,'build_opener',return_value=Upstream()):
                    self.assertEqual(post(token='wrong'),403)
                    self.assertEqual(post(model='other'),400)
                    self.assertEqual(ledger.count,0)
                    self.assertEqual(post(),200)
                    self.assertEqual(post(),403)
                self.assertEqual(len(observed),1)
                self.assertEqual(observed[0].full_url,module.ALLOWED_BASE+'/chat/completions')
                self.assertEqual(observed[0].get_header('Authorization'),'Bearer private-fixture-key')
                self.assertEqual(ledger.count,1)
            finally:
                server.shutdown();server.server_close();thread.join(timeout=5)


if __name__=='__main__':unittest.main()
