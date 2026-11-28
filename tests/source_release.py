"""Source-release contracts with ephemeral test keys and temporary Git repos only.

No real Taskloom release, maintainer identity, network or publication is used.
"""
import gzip
import base64
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PrivateFormat, PublicFormat, NoEncryption

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / 'examples/release/source_release.py'
SPEC = importlib.util.spec_from_file_location('taskloom_source_release_fixture', TOOL)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class SourceReleaseControls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '--quiet')
        self.git('config', 'user.name', 'Explicit source-release test fixture')
        self.git('config', 'user.email', 'source-release-fixture@example.invalid')
        # This release profile compares raw worktree bytes to immutable blobs;
        # fixture commits must not apply global EOL/clean filters at git add.
        self.git('config', 'core.autocrlf', 'false')
        (self.repo / '.gitignore').write_text('/target\n', encoding='utf-8')
        (self.repo / 'Cargo.toml').write_text(
            '[package]\nname="taskloom-source-fixture"\nversion="0.0.0-test"\nrust-version="1.86"\n'
            'license="MIT OR Apache-2.0"\n[features]\ndefault=[]\nruntime=[]\n', encoding='utf-8')
        (self.repo / 'README.md').write_text('Explicit non-production release fixture. 中文。\n', encoding='utf-8')
        (self.repo / 'src').mkdir()
        (self.repo / 'src/lib.rs').write_text('// Source identity fixture; never compiled or executed.\n', encoding='utf-8')
        self.commit()
        self.private = self.root / 'ephemeral-test-private.raw'
        self.public = self.root / 'ephemeral-test-public.raw'
        key = Ed25519PrivateKey.generate()
        self.private.write_bytes(key.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption()))
        if os.name != 'nt':
            self.private.chmod(0o600)
        self.public.write_bytes(key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw))
        self.bundle = self.root / 'bundle'
        self.signature = self.root / 'test-source-signature.json'

    def tearDown(self):
        self.directory.cleanup()

    def git(self, *args):
        run = subprocess.run(['git', '-C', str(self.repo), *args], capture_output=True,
                             timeout=20, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        self.assertEqual(run.returncode, 0, 'temporary Git fixture operation failed')
        return run.stdout.decode('utf-8').strip()

    def commit(self):
        self.git('add', '--all')
        self.git('commit', '--quiet', '-m', 'Explicit source-release test fixture, not a product release')

    def prepared(self):
        return release.prepare(self.repo, self.bundle)

    def signed(self):
        result = self.prepared()
        release.sign(self.repo, self.bundle, self.private, self.signature)
        return result

    def test_exact_clean_git_archive_checksum_signature_and_explicit_trust(self):
        result = self.signed()
        verified = release.verify(self.repo, self.bundle, self.signature, self.public)
        self.assertTrue(verified['verified_exact_source'])
        self.assertFalse(verified['quality_or_maintainer_identity_verified'])
        self.assertEqual(result['source'], {'commit': self.git('rev-parse', 'HEAD'),
                                          'tree': self.git('rev-parse', 'HEAD^{tree}'), 'clean': True})
        self.assertEqual(result['package']['msrv'], '1.86')
        self.assertEqual(result['package']['default_features'], [])
        artifact = (self.bundle / release.ARTIFACT).read_bytes()
        self.assertLessEqual(len(artifact), 16 * 1024 * 1024)
        self.assertEqual((self.bundle / release.CHECKSUM).read_text(), f'{release.sha(artifact)}  source.tar.gz\n')
        second = self.root / 'second-bundle'
        release.prepare(self.repo, second)
        self.assertEqual(artifact, (second / release.ARTIFACT).read_bytes())
        self.assertEqual(result, json.loads((second / release.MANIFEST).read_text()))
        self.assertEqual(self.git('status', '--porcelain'), '')
        command = subprocess.run([sys.executable, str(TOOL), 'verify', '--repo', str(self.repo),
                                  '--bundle', str(self.bundle), '--signature', str(self.signature),
                                  '--public-key', str(self.public)], capture_output=True, timeout=30)
        self.assertEqual(command.returncode, 0, 'actual CLI verification failed')
        self.assertTrue(json.loads(command.stdout)['verified_exact_source'])

    def test_changed_worktree_index_untracked_or_head_cannot_claim_clean(self):
        for mode in ('worktree', 'index', 'untracked'):
            with self.subTest(mode=mode):
                readme = self.repo / 'README.md'
                original = readme.read_bytes()
                if mode == 'untracked':
                    (self.repo / 'untracked.txt').write_text('not committed', encoding='utf-8')
                else:
                    readme.write_bytes(original + b'changed\n')
                    if mode == 'index':
                        self.git('add', 'README.md')
                with self.assertRaises(release.Rejected):
                    release.prepare(self.repo, self.root / ('bad-' + mode))
                if mode == 'untracked':
                    (self.repo / 'untracked.txt').unlink()
                else:
                    self.git('reset', '--quiet', 'HEAD', '--', 'README.md')
                    readme.write_bytes(original)
        self.signed()
        (self.repo / 'README.md').write_text('new committed source\n', encoding='utf-8')
        self.commit()
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, self.public)

    def test_archive_manifest_checksum_signature_and_wrong_key_tampering_fail(self):
        self.signed()
        cases = [self.bundle / release.ARTIFACT, self.bundle / release.MANIFEST,
                 self.bundle / release.CHECKSUM, self.signature]
        for path in cases:
            with self.subTest(path=path.name):
                original = path.read_bytes()
                damaged = bytearray(original)
                damaged[len(damaged) // 2] ^= 1
                path.write_bytes(damaged)
                with self.assertRaises(release.Rejected):
                    release.verify(self.repo, self.bundle, self.signature, self.public)
                path.write_bytes(original)
        other = self.root / 'another-ephemeral-public.raw'
        other.write_bytes(Ed25519PrivateKey.generate().public_key().public_bytes(Encoding.Raw, PublicFormat.Raw))
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, other)
        original_packet = json.loads(self.signature.read_text())
        packet = dict(original_packet)
        packet['signature_base64'] = base64.b64encode(bytes(64)).decode('ascii')
        self.signature.write_bytes(release.canonical(packet))
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, self.public)
        private = Ed25519PrivateKey.from_private_bytes(self.private.read_bytes())
        packet = dict(original_packet)
        packet['signature_base64'] = base64.b64encode(private.sign(
            b'taskloom-skill-registry-v1\x00' + (self.bundle / release.MANIFEST).read_bytes())).decode('ascii')
        self.signature.write_bytes(release.canonical(packet))
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, self.public)
        self.signature.write_bytes(release.canonical(original_packet))
        packet = json.loads(self.signature.read_text())
        packet['domain'] = 'taskloom-skill-registry-v1'
        self.signature.write_bytes(release.canonical(packet))
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, self.public)

    def test_source_identity_and_compatibility_claims_are_not_user_assertions(self):
        self.prepared()
        path = self.bundle / release.MANIFEST
        original = json.loads(path.read_text())
        for section, field, value in [('source', 'tree', '0' * 40), ('source', 'clean', False),
                                      ('source', 'commit', '0' * 40), ('package', 'msrv', '1.85'),
                                      ('package', 'default_features', ['runtime'])]:
            value_copy = json.loads(json.dumps(original))
            value_copy[section][field] = value
            path.write_bytes(release.canonical(value_copy))
            with self.assertRaises(release.Rejected):
                release.sign(self.repo, self.bundle, self.private, self.signature)
        path.write_bytes(release.canonical(original))
        content = path.read_bytes()
        path.write_bytes(content.replace(b'"format":', b'"format":"duplicate","format":', 1))
        with self.assertRaises(release.Rejected):
            release.sign(self.repo, self.bundle, self.private, self.signature)
        self.assertFalse(self.signature.exists())

    def test_create_only_outputs_ignored_location_and_fixed_size_caps(self):
        self.signed()
        for action in (lambda: release.prepare(self.repo, self.bundle),
                       lambda: release.sign(self.repo, self.bundle, self.private, self.signature),
                       lambda: release.sign(self.repo, self.bundle, self.private, self.repo / 'unignored-signature.json'),
                       lambda: release.prepare(self.repo, self.repo / 'unignored-release')):
            with self.assertRaises(release.Rejected):
                action()
        target = self.repo / 'target'
        target.mkdir()
        release.prepare(self.repo, target / 'ignored-release')
        self.assertEqual(self.git('status', '--porcelain'), '')
        with mock.patch.object(release, 'MAX_ARTIFACT', 32):
            with self.assertRaises(release.Rejected):
                release.verified_bundle(self.repo, self.bundle)
        artifact_path = self.bundle / release.ARTIFACT
        original = artifact_path.read_bytes()
        artifact_path.write_bytes(b'x' * (16 * 1024 * 1024 + 1))
        with self.assertRaises(release.Rejected):
            release.verified_bundle(self.repo, self.bundle)
        with mock.patch.object(release, 'MAX_TAR', 1024):
            artifact_path.write_bytes(gzip.compress(b'x' * 4096))
            with self.assertRaises(release.Rejected):
                release.verified_bundle(self.repo, self.bundle)
        artifact_path.write_bytes(original)

    def test_compiled_secrets_symlinks_and_submodules_are_not_source_release_entries(self):
        # This is a conservative file/profile gate, not a general secret scanner.
        for name in ('program.exe', 'private.key', 'credentials.json'):
            with self.subTest(name=name):
                path = self.repo / name
                path.write_bytes(b'explicit test bytes, not a real secret or executable')
                self.commit()
                with self.assertRaises(release.Rejected):
                    release.prepare(self.repo, self.root / ('refuse-' + name))
                path.unlink()
                self.commit()
        if os.name != 'nt':
            (self.repo / 'linked-source').symlink_to('README.md')
            self.commit()
            with self.assertRaises(release.Rejected):
                release.prepare(self.repo, self.root / 'refuse-link')
            (self.repo / 'linked-source').unlink()
            self.commit()
        self.git('update-index', '--add', '--cacheinfo', f'160000,{self.git("rev-parse", "HEAD")},submodule')
        self.git('commit', '--quiet', '-m', 'Explicit gitlink rejection fixture')
        with self.assertRaises(release.Rejected):
            release.prepare(self.repo, self.root / 'refuse-gitlink')

    def test_keys_are_explicit_bounded_outside_bundle_and_never_printed_or_created(self):
        self.signed()
        private_before = self.private.read_bytes()
        embedded = self.bundle / 'offered-public.raw'
        embedded.write_bytes(self.public.read_bytes())
        with self.assertRaises(release.Rejected):
            release.verify(self.repo, self.bundle, self.signature, embedded)
        bad_private = self.root / 'oversized-private.raw'
        bad_private.write_bytes(private_before + b'oversized')
        run = subprocess.run([sys.executable, str(TOOL), 'sign', '--repo', str(self.repo), '--bundle', str(self.bundle),
                              '--private-key', str(bad_private), '--out', str(self.root / 'no-signature.json')],
                             capture_output=True, timeout=30)
        self.assertEqual(run.returncode, 2)
        self.assertNotIn(private_before.hex().encode(), run.stdout + run.stderr)
        self.assertNotIn(private_before, run.stdout + run.stderr)
        self.assertFalse((self.root / 'no-signature.json').exists())
        self.assertEqual(self.private.read_bytes(), private_before)
        tracked_private = self.repo / 'ephemeral-private.raw'
        tracked_private.write_bytes(private_before)
        self.commit()
        another = self.root / 'tracked-key-bundle'
        release.prepare(self.repo, another)
        with self.assertRaises(release.Rejected):
            release.sign(self.repo, another, tracked_private, self.root / 'must-not-sign.json')

    def test_readonly_git_inspection_does_not_execute_source_filters_or_fsmonitor(self):
        (self.repo / '.gitattributes').write_text('*.rs filter=forbidden\n', encoding='utf-8')
        self.commit()
        marker = self.root / 'filter-was-executed'
        # A command that would leave an observable marker if Git source filters
        # or fsmonitor were invoked. The release tool only reads raw objects.
        forbidden = f'{sys.executable} -c "open(r\'{marker}\',\'w\').write(\'unexpected\')"'
        self.git('config', 'filter.forbidden.clean', forbidden)
        self.git('config', 'filter.forbidden.required', 'true')
        self.git('config', 'core.fsmonitor', forbidden)
        release.prepare(self.repo, self.bundle)
        self.assertFalse(marker.exists())

    def test_actual_git_cargo_msrv_and_default_feature_contract_must_match_profile(self):
        cargo = self.repo / 'Cargo.toml'
        original = cargo.read_text()
        for i, changed in enumerate((original.replace('rust-version="1.86"', 'rust-version="1.85"'),
                                     original.replace('default=[]', 'default=["runtime"]'))):
            cargo.write_text(changed, encoding='utf-8')
            self.commit()
            with self.assertRaises(release.Rejected):
                release.prepare(self.repo, self.root / f'bad-compatibility-{i}')
        self.assertFalse((self.root / 'bad-compatibility-0').exists())
        self.assertFalse((self.root / 'bad-compatibility-1').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
