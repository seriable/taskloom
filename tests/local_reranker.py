"""Model-directory identity controls; no weights or model requests are used."""
import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]/'examples/local_models'
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location('local_reranker', ROOT/'serve_reranker.py')
reranker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reranker)


class ArtifactIdentity(unittest.TestCase):
    def fixture(self, folder):
        body = b'{"unit":"synthetic artifact guard, not actual model"}'
        (folder/'config.json').write_bytes(body)
        return [{'path': 'config.json', 'bytes': len(body), 'sha256': hashlib.sha256(body).hexdigest()}]

    def test_exact_bytes_accept_and_unbound_template_rejects(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            files = self.fixture(folder)
            reranker.verify_artifacts(folder, files)
            (folder/'chat_template.jinja').write_text('unbound altered model conditioning', encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'unbound artifact'):
                reranker.verify_artifacts(folder, files)

    def test_changed_bound_file_and_additional_adapter_directory_reject(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            files = self.fixture(folder)
            (folder/'config.json').write_bytes(b'wrong')
            with self.assertRaisesRegex(ValueError, 'locked identity'):
                reranker.verify_artifacts(folder, files)
            self.fixture(folder)
            (folder/'adapter').mkdir()
            with self.assertRaisesRegex(ValueError, 'unbound artifact'):
                reranker.verify_artifacts(folder, files)


if __name__ == '__main__':
    unittest.main()
