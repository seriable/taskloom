"""Comparability controls, with explicit synthetic recordings and no model calls."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1]/'examples/benchmarks/compare_parity.py'


class ObservedParityControls(unittest.TestCase):
    def prepare(self, root):
        native, other = root/'native', root/'other'
        native.mkdir()
        (other/'case').mkdir(parents=True)
        config = {'model': 'synthetic-unit-model', 'temperature': 0.0, 'max_output_tokens': 8,
                  'tools': [], 'limits': {'max_steps': 2}, 'policy': {'allowed_tools': [], 'grants': []},
                  'approvals': {}, 'context': {}}
        state = {'config': config, 'report': {'messages': [{'role': 'system', 'content': 'unit system'},
                  {'role': 'user', 'content': 'unit prompt'}], 'contexts': [
                  {'step': 1, 'summary': None, 'retained_indices': [0, 1]}], 'budget': {'uncertain_attempts': 0}}}
        baseline = {'suite_fingerprint': 'unit-only', 'requested': 1, 'not_run': 0,
                    'configuration': {'max_total_cost_micros': 1},
                    'cases': [{'id': 'case', 'trajectory_path': 'state.json', 'usage': {'requests': 1}, 'status': 'passed'}]}
        candidate = {'suite_fingerprint': 'unit-only', 'requested': 1, 'not_run': 0, 'max_total_cost_micros': 1,
                     'cases': [{'id': 'case', 'status': 'passed', 'applied_contract': copy.deepcopy(config)}]}
        request = {'model': config['model'], 'temperature': 0.0, 'max_tokens': 8, 'tools': [],
                   'messages': copy.deepcopy(state['report']['messages'])}
        files = {native/'report.json': baseline, native/'suite.json': {'cases': [{'id': 'case'}]},
                 native/'state.json': state, other/'report.json': candidate,
                 other/'execution-provenance.json': {'scope': 'synthetic test, never actual execution'},
                 other/'case/model-api.json': [{'request': request, 'http_attempts': [{'status': 200}]}],
                 other/'case/result.json': candidate['cases'][0]}
        for path, value in files.items():
            path.write_text(json.dumps(value), encoding='utf-8')
        (other/'harness.py').write_text('# synthetic unit fixture only\n', encoding='utf-8')
        return native, other, files

    def run_comparison(self, native, other, out, strict=True):
        return subprocess.run([sys.executable, str(SCRIPT), str(native), str(other), str(out)] +
                              (['--require-applied-contract'] if strict else []), capture_output=True)

    def test_complete_equal_contract_binds_original_recordings(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native, other, _ = self.prepare(root)
            out = root/'audit.json'
            self.assertEqual(self.run_comparison(native, other, out).returncode, 0)
            report = json.loads(out.read_text(encoding='utf-8'))
            self.assertEqual(report['case_denominator'], 1)
            self.assertTrue(report['all_applied_contracts_match'])
            self.assertEqual(len(report['evidence']), 8)
            self.assertTrue(all(len(item['sha256']) == 64 for item in report['evidence']))
            self.assertNotEqual(self.run_comparison(native, other, out).returncode, 0)

    def test_missing_case_is_not_silently_dropped(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native, other, files = self.prepare(root)
            value = files[other/'report.json']
            value['cases'] = []
            (other/'report.json').write_text(json.dumps(value), encoding='utf-8')
            self.assertNotEqual(self.run_comparison(native, other, root/'audit.json').returncode, 0)

    def test_changed_grants_fail_even_with_identical_wire_and_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native, other, files = self.prepare(root)
            value = files[other/'report.json']
            value['cases'][0]['applied_contract']['policy']['grants'] = ['not-the-original-grant']
            (other/'report.json').write_text(json.dumps(value), encoding='utf-8')
            out = root/'audit.json'
            self.assertNotEqual(self.run_comparison(native, other, out).returncode, 0)
            report = json.loads(out.read_text(encoding='utf-8'))
            self.assertTrue(report['all_conditioning_matches'])
            self.assertFalse(report['all_applied_contracts_match'])

    def test_historical_missing_contract_remains_unattested(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native, other, files = self.prepare(root)
            value = files[other/'report.json']
            del value['cases'][0]['applied_contract']
            (other/'report.json').write_text(json.dumps(value), encoding='utf-8')
            out = root/'historical.json'
            self.assertEqual(self.run_comparison(native, other, out, strict=False).returncode, 0)
            self.assertFalse(json.loads(out.read_text(encoding='utf-8'))['all_applied_contracts_match'])
            self.assertNotEqual(self.run_comparison(native, other, root/'strict.json').returncode, 0)


if __name__ == '__main__':
    unittest.main()
