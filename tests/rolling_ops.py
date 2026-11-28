"""Pure rollout admission refusals; synthetic files are never executed.

Real old/new compatibility and signal behavior have separate POSIX receipts.
"""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

OPS=Path(__file__).resolve().parents[1]/'examples/operations'
sys.path.insert(0,str(OPS))
import rolling_rehearsal as rolling
from binary_witness import source_files


class Admission(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        old=self.root/'old';(old/'source').mkdir(parents=True);(old/'source/Cargo.lock').write_text('old locked dependencies');(old/'source.tar').write_bytes(b'unit-only archive; not real build evidence')
        current=self.root/'current';(current/'src/runtime').mkdir(parents=True);(current/'Cargo.toml').write_text('unit package');(current/'Cargo.lock').write_text('current locked dependencies');(current/'src/runtime/database.rs').write_text('pub const DATABASE_SCHEMA_VERSION: i64 = 3;')
        self.old_bin=old/'taskloom';self.old_bin.write_bytes(b'\x7fELFold-synthetic-never-executed')
        self.new_bin=current/'taskloom';self.new_bin.write_bytes(b'\x7fELFnew-synthetic-never-executed')
        self.old={'status':'passed','completed':True,'source_commit':rolling.OLD_COMMIT,'features':['app','database'],'archive_sha256':rolling.sha(old/'source.tar'),'cargo_lock_sha256':rolling.sha(old/'source/Cargo.lock'),'schema_version':3,'rustc':'same-test-compiler','binary':{'path':str(self.old_bin),'sha256':rolling.sha(self.old_bin),'version':'unit-only same preview'}}
        files=source_files(current)
        self.new={'kind':'host_frozen_source_build_witness','status':'passed','completed':True,'features':'all-features','source_root':str(current),'source_files':files,'source_sha256':rolling.canonical(files),'schema_version':3,'rustc':'same-test-compiler','binary':{'path':str(self.new_bin),'sha256':rolling.sha(self.new_bin),'version':'unit-only same preview'}}
        self.old_path=old/'report.json';self.new_path=current/'report.json'
    def tearDown(self):self.temp.cleanup()
    def check(self):
        self.old_path.write_text(json.dumps(self.old));self.new_path.write_text(json.dumps(self.new))
        with patch('subprocess.Popen',side_effect=AssertionError('must not launch a process')),patch('sqlite3.connect',side_effect=AssertionError('must not open a database')):
            return rolling.verify_sources(self.old_path,rolling.sha(self.old_path),self.new_path,rolling.sha(self.new_path))
    def test_metadata_admission_has_no_process_database_or_model_side_effect(self):self.check()
    def test_wrong_archive_source_commit_is_refused(self):
        self.old['source_commit']='0'*40
        with self.assertRaisesRegex(ValueError,'source/capability'):self.check()
    def test_changed_archived_lock_is_refused(self):
        (self.old_path.parent/'source/Cargo.lock').write_text('changed')
        with self.assertRaisesRegex(ValueError,'archive/lock'):self.check()
    def test_changed_current_source_is_refused(self):
        (self.new_path.parent/'src/runtime/database.rs').write_text('changed after compiler witness')
        with self.assertRaisesRegex(ValueError,'frozen source changed'):self.check()
    def test_binary_substitution_is_refused(self):
        self.new_bin.write_bytes(b'\x7fELFreplaced')
        with self.assertRaisesRegex(ValueError,'executable differs'):self.check()
    def test_reusing_one_binary_as_old_and_new_is_refused(self):
        self.new['binary']=copy.deepcopy(self.old['binary'])
        with self.assertRaisesRegex(ValueError,'distinct actual binaries'):self.check()
    def test_schema_mismatch_cannot_enter_a_migration(self):
        self.new['schema_version']=4
        with self.assertRaisesRegex(ValueError,'no migration'):self.check()
    def test_unapproved_capability_build_is_refused(self):
        self.new['features']='unapproved profile'
        with self.assertRaisesRegex(ValueError,'capability witness'):self.check()
    def test_wrong_explicit_witness_digest_refuses_before_execution(self):
        self.check()
        with self.assertRaisesRegex(ValueError,'approved SHA'):rolling.verify_sources(self.old_path,'0'*64,self.new_path,rolling.sha(self.new_path))


if __name__=='__main__':unittest.main()
