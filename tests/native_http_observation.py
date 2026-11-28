"""Offline HTTP read-observation recovery; effectful submissions never replay."""
import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch
import urllib.request
import urllib.error

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('native_http',ROOT/'examples/benchmarks/native_retrieval.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)


class HttpObservationControls(unittest.TestCase):
    def test_get_recovers_after_transient_transport_failure(self):
        response=io.BytesIO(b'{"state":"persisted"}')
        with patch.object(module.urllib.request,'urlopen',side_effect=[urllib.error.URLError('connection timeout'),response]) as opened,patch.object(module.time,'sleep'):
            self.assertEqual(module.request_json(urllib.request.Request('http://127.0.0.1/run')),{'state':'persisted'})
        self.assertEqual(opened.call_count,2)
        self.assertTrue(all(c.kwargs['timeout']==10 for c in opened.call_args_list))

    def test_post_unknown_result_is_never_repeated(self):
        with patch.object(module.urllib.request,'urlopen',side_effect=urllib.error.URLError('lost response')) as opened:
            with self.assertRaises(urllib.error.URLError):
                module.request_json(urllib.request.Request('http://127.0.0.1/run',data=b'{}'))
        self.assertEqual(opened.call_count,1)

    def test_get_attempts_are_bounded_and_http_denials_not_retried(self):
        request=urllib.request.Request('http://127.0.0.1/run')
        with patch.object(module.urllib.request,'urlopen',side_effect=TimeoutError()) as opened,patch.object(module.time,'sleep'):
            with self.assertRaises(TimeoutError):module.request_json(request)
        self.assertEqual(opened.call_count,3)
        error=urllib.error.HTTPError(request.full_url,403,'denied',{},io.BytesIO(b'forbidden'))
        with patch.object(module.urllib.request,'urlopen',side_effect=error) as opened:
            with self.assertRaisesRegex(RuntimeError,'HTTP 403'):module.request_json(request)
        self.assertEqual(opened.call_count,1)


if __name__=='__main__':unittest.main()
