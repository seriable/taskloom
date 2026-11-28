"""Scoring controls only; these tests never invoke a model or claim native quality."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('native_claim_support', Path(__file__).resolve().parents[1] / 'examples/benchmarks/native_claim_support.py')
import sys
sys.path.insert(0, str(Path(spec.origin).parent))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ScoringControls(unittest.TestCase):
    def test_generated_assessment_cannot_hide_failed_final_or_missing_answers(self):
        gates={'expected_cases':2,'minimum_cited_support':.5,'minimum_corpus_factuality':.5,'max_errors':0}
        rows=[{'native':{'cited_support':'supported','corpus_factuality':'supported'}},
              {'is_final':True,'native':{'cited_support':'insufficient','corpus_factuality':'supported'}}]
        measures,checks=module.score_generated(rows,gates)
        self.assertEqual(measures['cited_support_supported'],.5)
        self.assertFalse(checks['final_answer_supported'])
        rows[1]['error']='invalid final contract'
        measures,checks=module.score_generated(rows,gates)
        self.assertEqual(measures['corpus_factuality_supported'],.5)
        self.assertFalse(checks['errors'])
        self.assertFalse(module.score_generated(rows[:1],gates)[1]['fixed_case_count'])
    def test_citation_role_context_cannot_read_corpus_or_expected_labels(self):
        attached=[{'chunk_id':'a'*64,'quote':'The switch is off.'}]
        corpus=[{'document_id':'private-to-corpus-judge','body':'A separate source says the car is blue.'}]
        request=module.separate_judge_request(3,'The car is blue.',attached,corpus)
        first,second=request['definition']['nodes']
        self.assertEqual(first['params'],{'claim':'The car is blue.','evidence':attached})
        self.assertNotIn('private-to-corpus-judge',str(first))
        self.assertEqual(second['params']['evidence'],corpus)
        self.assertEqual(first['depends_on'],[])
        self.assertEqual(second['depends_on'],[])
        self.assertIsNone(request['input'])
        self.assertNotIn('expected',str(request))
    def test_error_and_missing_native_records_cannot_pass_or_leave_denominator(self):
        gates = {'minimum_cited_support_agreement': .85, 'minimum_corpus_factuality_agreement': .85,
                 'required_known_contradictions_rejected': 1, 'max_errors': 0}
        expected = {'expected_cited_support': 'contradicted', 'expected_corpus_factuality': 'contradicted'}
        native = {'cited_support': 'contradicted', 'corpus_factuality': 'contradicted'}
        rows = [dict(expected, native=native), dict(expected, native=native, error='invalid terminal JSON contract'), dict(expected)]
        measurements, checks, confusion = module.score_controls(rows, gates)
        self.assertEqual(measurements['cited_agrees'], 1 / 3)
        self.assertEqual(measurements['known_contradictions_rejected'], 1 / 3)
        self.assertFalse(any(checks.values()))
        self.assertEqual(confusion['corpus_factuality']['contradicted'], {'contradicted': 1, 'error': 2})

    def test_citation_agreement_and_corpus_agreement_are_separate(self):
        gates = {'minimum_cited_support_agreement': 1, 'minimum_corpus_factuality_agreement': 1,
                 'required_known_contradictions_rejected': 1, 'max_errors': 0}
        rows = [
            {'expected_cited_support': 'insufficient', 'expected_corpus_factuality': 'supported',
             'native': {'cited_support': 'supported', 'corpus_factuality': 'supported'}},
            {'expected_cited_support': 'contradicted', 'expected_corpus_factuality': 'contradicted',
             'native': {'cited_support': 'contradicted', 'corpus_factuality': 'contradicted'}}]
        measurements, checks, _ = module.score_controls(rows, gates)
        self.assertEqual(measurements['cited_agrees'], .5)
        self.assertEqual(measurements['corpus_agrees'], 1)
        self.assertFalse(checks['cited_support_agreement'])
        self.assertTrue(checks['corpus_factuality_agreement'])


if __name__ == '__main__':
    unittest.main()
