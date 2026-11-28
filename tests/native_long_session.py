"""Offline length-audit controls, not real model task-quality evidence."""
import importlib.util
from pathlib import Path
import sys
import unittest

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'examples/benchmarks'))
spec=importlib.util.spec_from_file_location('long_session',ROOT/'examples/benchmarks/native_long_session.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)


class LengthControls(unittest.TestCase):
    def test_original_answer_boundary_and_host_rendering(self):
        verified={'report':{'text':'host '+('rendered '*500)},'rendering':{'original':{'text':'word '*150}}}
        self.assertEqual(module.answer_length(verified,1),{'words':150,'user_limit':150,'passed':True})
        verified['rendering']['original']['text']+='extra'
        self.assertFalse(module.answer_length(verified,1)['passed'])
        self.assertTrue(module.answer_length(verified,12)['passed'])

    def test_cited_report_fallback_and_final_limit(self):
        verified={'report':{'text':'word\n'*301}}
        self.assertFalse(module.answer_length(verified,12)['passed'])
        verified['report']['text']='word\t'*300
        self.assertTrue(module.answer_length(verified,12)['passed'])


if __name__=='__main__':unittest.main()
