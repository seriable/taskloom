"""Offline scope-screen controls; no model-quality claims or API requests."""
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location('scope_precision', Path(__file__).resolve().parents[1] / 'examples/benchmarks/native_scope_precision.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ScopeControls(unittest.TestCase):
    def test_exact_original_offsets_and_no_automatic_verdict(self):
        text = 'Memory: When condition C holds, inspect the saved run. This remedy applies only to C.'
        result = module.screen_scope(text, [{'id': 'source', 'text': 'When C holds, inspect the saved run.'}])
        self.assertTrue(result['review_required'])
        found = result['findings'][0]
        self.assertEqual(text[found['match_start']:found['match_end']], 'applies only')
        self.assertEqual(text[found['paragraph_start']:found['paragraph_end']], text)
        self.assertFalse(result['answer_modified'])
        self.assertIsNone(result['semantic_verdict'])

    def test_matching_source_word_is_not_automatic_support(self):
        result = module.screen_scope('This procedure applies only to C.',
            [{'id': 'source', 'text': 'Only the current slot is released.'}])
        self.assertTrue(result['review_required'])
        self.assertEqual(result['possible_source_leads'][0]['text'], 'Only the current slot is released.')
        self.assertIsNone(result['semantic_verdict'])

    def test_supported_exclusion_still_requires_review(self):
        text = 'The procedure never applies to D.'
        result = module.screen_scope(text, [{'id': 'source', 'text': text}])
        self.assertTrue(result['review_required'])
        self.assertIsNone(result['semantic_verdict'])

    def test_scoped_gap_and_conditional_claim_are_not_rewritten(self):
        text = 'The excerpt states that when C holds, inspect the saved run. It does not establish availability for D.'
        result = module.screen_scope(text, [])
        self.assertFalse(result['review_required'])
        self.assertIsNone(result['semantic_verdict'])
        self.assertIn('Absence of a match does not establish support', result['limits'])


if __name__ == '__main__':
    unittest.main()
