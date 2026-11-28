"""Offline negative controls for managed DR asset/secret/authority boundaries."""
import importlib.util
import json
from pathlib import Path
import tempfile
import os
import subprocess
import unittest


SOURCE = Path(__file__).resolve().parents[1] / 'examples' / 'operations' / 'managed_dr.py'
SPEC = importlib.util.spec_from_file_location('managed_dr', SOURCE)
dr = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dr)


class ManagedDrControls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.inventory = {}
        for category in dr.CATEGORIES:
            (self.root / category).mkdir()
            self.inventory[category] = {'paths': [category], 'absent_reason': ''}

    def material(self):
        (self.root / 'config').mkdir()
        dr.private_save(self.root / 'config' / 'project.json',
                        {'state_dir': '../state', 'database': {'url_env': 'TASKLOOM_DR_DATABASE_URL'},
                         'provider': {'type': 'openai', 'key_env': 'FIXTURE_API_KEY'}})
        dr.private_save(self.root / 'secrets' / 'runtime-env.json', {'FIXTURE_API_KEY': 'private-fixture-value'})
        dr.private_save(self.root / 'secret_refs' / 'runtime.json',
                        {'version': 1, 'names': ['FIXTURE_API_KEY', 'TASKLOOM_DR_DATABASE_URL']})

    def test_full_inventory_detects_unclassified_and_overlapping_assets(self):
        dr.private_save(self.root / 'state' / 'index.json', {'actual': 'bytes'})
        values = dr.files(self.root, self.inventory)
        self.assertEqual(values['state/index.json']['category'], 'state')
        dr.private_save(self.root / 'undeclared.json', {'forgotten': True})
        with self.assertRaises(dr.Refused):
            dr.files(self.root, self.inventory)
        (self.root / 'undeclared.json').unlink()
        self.inventory['configuration']['paths'].append('state')
        with self.assertRaises(dr.Refused):
            dr.files(self.root, self.inventory)

    def test_all_independent_ledgers_need_an_explicit_declaration(self):
        self.inventory.pop('signed_registry')
        with self.assertRaises(dr.Refused):
            dr.files(self.root, self.inventory)

    def test_missing_runtime_secret_reference_refuses_instead_of_host_inheritance(self):
        self.material()
        self.assertEqual(dr.runtime_material(self.root), {'FIXTURE_API_KEY': 'private-fixture-value'})
        (self.root / 'secret_refs' / 'runtime.json').write_text(json.dumps({'version': 1, 'names': ['TASKLOOM_DR_DATABASE_URL']}))
        with self.assertRaises(dr.Refused):
            dr.runtime_material(self.root)

    def test_selected_environment_cannot_inject_loader_or_proxy_configuration(self):
        self.material()
        for name in ['LD_PRELOAD', 'PATH', 'HTTPS_PROXY', 'DOCKER_HOST', 'NODE_OPTIONS', 'RUST_LOG']:
            with self.subTest(name=name):
                (self.root / 'secrets' / 'runtime-env.json').write_text(json.dumps({name: 'private-fixture-value'}))
                (self.root / 'secret_refs' / 'runtime.json').write_text(json.dumps({'version': 1, 'names': sorted([name, 'TASKLOOM_DR_DATABASE_URL'])}))
                with self.assertRaises(dr.Refused):
                    dr.runtime_material(self.root)

    def test_symlink_and_path_traversal_refused(self):
        self.inventory['state']['paths'] = ['../outside']
        with self.assertRaises(dr.Refused):
            dr.files(self.root, self.inventory)
        self.inventory['state']['paths'] = ['state']
        link = self.root / 'state' / 'link'
        try:
            link.symlink_to(self.root / 'secret_refs', target_is_directory=True)
        except OSError:
            if os.name != 'nt':
                raise
            # Windows junctions need no symlink privilege and are equally
            # capable of escaping a backup root. Exercise that actual hazard.
            def ps_literal(path):
                return "'" + str(path).replace("'", "''") + "'"
            result = subprocess.run(['powershell', '-NoProfile', '-Command',
                'New-Item -ItemType Junction -Path ' + ps_literal(link) + ' -Target '
                + ps_literal(self.root / 'secret_refs') + ' | Out-Null'], capture_output=True)
            if result.returncode:
                self.skipTest('host cannot create symlinks or junctions')
        with self.assertRaises(dr.Refused):
            dr.files(self.root, self.inventory)

    def test_untrusted_identifiers_and_sql_null_refused(self):
        for name in ['a; DROP TABLE x', '../x', '', 'a"x', 'A', 'a' * 64]:
            with self.assertRaises(dr.Refused):
                dr.identifier(name)
        with self.assertRaises(dr.Refused):
            dr.literal('x\0y')
        self.assertEqual(dr.literal("a'b"), "'a''b'")

    def test_private_backup_flush_handles_copied_readonly_files(self):
        path = self.root / 'state' / 'readonly.json'
        path.write_bytes(b'private fixture bytes')
        path.chmod(0o400)
        before = dr.files(self.root, self.inventory)
        try:
            dr.flush_tree(self.root)
            self.assertEqual(dr.files(self.root, self.inventory), before)
        finally:
            path.chmod(0o600)

    def test_runtime_url_cannot_smuggle_an_unfenced_administrator_or_region(self):
        path = self.root / 'secrets' / 'deployment.json'
        value = {'role': 'app', 'database': 'project', 'url': 'postgres://app:fixture@a:5432/project?sslmode=disable'}
        dr.private_save(path, value)
        self.assertEqual(dr.deployment_material(self.root, 'a', 'project', 'app'), value)
        for url in ['postgres://taskloom_accept:fixture@a:5432/project?sslmode=disable',
                    'postgres://app:fixture@b:5432/project?sslmode=disable',
                    'postgres://app:fixture@a:5432/old_project?sslmode=disable',
                    'postgres://app:fixture@a:5432/project?sslmode=disable&options=-csearch_path=other']:
            path.write_text(json.dumps(dict(value, url=url)))
            with self.assertRaises(dr.Refused):
                dr.deployment_material(self.root, 'a', 'project', 'app')


if __name__ == '__main__':
    unittest.main()
