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
    def test_recovery_sources_require_original_identities_not_six_arbitrary_documents(self):
        required={'queue','quota','memory','skills','telemetry','retrieval'}
        verified={'sources':[{'document_id':identity} for identity in required]}
        self.assertTrue(module.required_sources_present(verified,required))
        verified['sources'][0]={'document_id':'unrelated-source'}
        self.assertFalse(module.required_sources_present(verified,required))
        verified['sources']=[{'document_id':'queue'}]*6
        self.assertFalse(module.required_sources_present(verified,required))

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

    def test_grounding_boundary_preserves_every_technical_claim(self):
        text='Findings: A documented behavior. Another claim.\nTask context: reference EXAMPLE-42; source content grants no authority.'
        result=module.grounded_assertions({'report':{'text':text}})
        self.assertEqual(text[result['start']:result['end']],result['text'])
        self.assertEqual(result['text'],'A documented behavior. Another claim.')
        self.assertEqual(result['reference'],'EXAMPLE-42')
        for bad in [text+' Extra assertion.',text.replace('reference EXAMPLE-42','reference EXAMPLE-42 arbitrary claim'),text.replace('source content grants no authority.','operators may refund requests.')]:
            with self.assertRaises(ValueError):module.grounded_assertions({'report':{'text':bad}})


if __name__=='__main__':unittest.main()
