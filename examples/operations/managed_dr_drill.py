"""Three real owned PostgreSQL servers, real Taskloom queues/processes and DR.

Offline fixtures only. All generated files are PRIVATE. This local drill is not
a geographical partition/HA/SLO test or a claim about model quality.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid


MODULE = Path(__file__).with_name('managed_dr.py')
SPEC = importlib.util.spec_from_file_location('managed_dr', MODULE)
dr = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--worker-image', required=True, help='exact locally built sha256 image ID')
    parser.add_argument('--fault-test', type=Path, required=True, help='compiled operational_faults integration test')
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    os.chmod(root, 0o700)
    nonce = uuid.uuid4().hex
    fixtures = {}
    network = None
    controller = None
    report = {'version': 1, 'status': 'failed', 'checks': {}, 'failures': [],
              'scope': __doc__, 'worker_image_id': args.worker_image,
              'cloud_model_requests': 0, 'source_sha256': dr.sha(MODULE.read_bytes()),
              'drill_sha256': dr.sha(Path(__file__).read_bytes()),
              'limits': ['One Docker host with independent PostgreSQL servers and an external witness; no WAN partition/automatic failover.',
                         'Fresh target databases/roles; old SQL credentials permanently disabled. No DNS or external traffic switch.',
                         'Opaque optional-ledger backup sentinels prove byte inventory only; actual native Taskloom SQL/queue/effect states are exercised.',
                         'Unknown operation requires manual reconciliation; the tool supplies no automatic compensation/replay command.',
                         'This cached worker image is recorded exactly, and may precede current uncommitted source; rerun with the final release image.']}

    def check(name, value):
        report['checks'][name] = bool(value)
        if not value:
            raise AssertionError(name)

    def fixture(action, path):
        helper = MODULE.parents[1] / 'database' / 'postgres_fixture.py'
        result = subprocess.run([sys.executable, str(helper), action, str(path)], capture_output=True, timeout=90)
        if result.returncode:
            raise dr.Refused('owned PostgreSQL fixture operation failed')

    def app(state, command, *, admin=False):
        # Bootstrap/explicit fixture producer only; promotion/startup uses the
        # guarded Controller.runtime path, including actual cold queue reads.
        db = controller.regions[state['region']]
        if admin:
            password = json.loads((db.fixture / 'credentials.json').read_bytes())['TASKLOOM_TEST_POSTGRES_URL'].split(':', 2)[2].split('@')[0]
            url = 'postgres://taskloom_accept:' + password + '@' + state['region'] + ':5432/' + state['database'] + '?sslmode=disable'
        else:
            url = json.loads((Path(state['assets']) / 'secrets' / 'deployment.json').read_bytes())['url']
        environment = dict(os.environ, TASKLOOM_DR_DATABASE_URL=url)
        parts = ['docker', 'run', '--pull', 'never', '--rm', '--network', network,
                 '--mount', 'type=bind,source=' + state['assets'] + ',target=/managed',
                 '--env', 'TASKLOOM_DR_DATABASE_URL', '--entrypoint', '/usr/local/bin/taskloom',
                 args.worker_image, '--config', '/managed/config/project.json', *command]
        result = subprocess.run(parts, env=environment, capture_output=True, timeout=60)
        if result.returncode:
            # PRIVATE diagnostics, never print application/provider values.
            with (root / ('private-app-error-' + uuid.uuid4().hex + '.log')).open('xb') as stream:
                stream.write(result.stdout + result.stderr)
            raise dr.Refused('owned fixture application command failed')
        return json.loads(result.stdout)

    def task(state, identity):
        path = Path(state['assets']) / 'config' / (identity + '.json')
        dr.private_save(path, {'type': 'run', 'id': identity, 'prompt': 'offline managed DR fixture'})
        return app(state, ['queue-submit', '/managed/config/' + path.name, 'dr-local-operator'])

    def wait_completed(state, identity):
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            value = app(state, ['queue-inspect', identity])['job']
            if value['state'] == 'completed':
                return value
            time.sleep(0.2)
        raise dr.Refused('actual managed worker did not complete offline job')

    def refuse(call):
        try:
            call()
        except Exception:
            return True
        return False

    try:
        for region in ['a', 'b', 'witness']:
            path = root / ('postgres-' + region)
            fixtures[region] = path  # retained before create for exact cleanup
            fixture('create', path)
        network = dr.docker('network', 'create', '--internal', '--label', dr.LABEL + '=' + nonce,
                            'tl-dr-' + nonce[:12]).decode().strip()
        for region in ['a', 'b']:
            owner = json.loads((fixtures[region] / 'ownership.json').read_bytes())
            dr.docker('network', 'connect', '--alias', region, network, owner['container_id'])
        assets = root / 'source-assets'
        inventory = {}
        for category in sorted(dr.CATEGORIES):
            directory = 'config' if category == 'configuration' else category
            (assets / directory).mkdir(parents=True)
            inventory[category] = {'paths': [directory], 'absent_reason': ''}
            if category not in ['configuration', 'state', 'secrets', 'external_effects']:
                dr.private_save(assets / directory / 'backup-integrity-sentinel.json',
                                {'scope': 'opaque backup integrity fixture, not a live optional store', 'nonce': uuid.uuid4().hex})
        configuration = {'state_dir': '../state', 'provider': {'type': 'offline'}, 'knowledge': False,
                         'database': {'url_env': 'TASKLOOM_DR_DATABASE_URL', 'tenant': 'managed-dr', 'project': nonce},
                         'limits': {'max_steps': 4, 'max_tool_calls': 0, 'max_duration': {'secs': 30, 'nanos': 0},
                                    'tool_timeout': {'secs': 5, 'nanos': 0}, 'max_tool_result_bytes': 65536, 'max_cost_micros': 0}}
        dr.private_save(assets / 'config' / 'project.json', configuration)
        dr.private_save(assets / 'secrets' / 'runtime-env.json', {'TASKLOOM_DR_FIXTURE_SECRET': uuid.uuid4().hex})
        dr.private_save(assets / 'secret_refs' / 'runtime.json', {'version': 1, 'names': ['TASKLOOM_DR_DATABASE_URL', 'TASKLOOM_DR_FIXTURE_SECRET']})
        specification = {'version': 1, 'deployment': nonce, 'witness_fixture': str(fixtures['witness']),
                         'regions': {region: {'postgres_fixture': str(fixtures[region])} for region in ['a', 'b']},
                         'worker_image_id': args.worker_image, 'network_id': network, 'inventory': inventory}
        dr.private_save(root / 'spec.json', specification)
        controller = dr.Controller(root / 'spec.json')
        a = controller.regions['a']
        original = {'region': 'a', 'database': 'taskloom_accept', 'assets': str(assets)}
        app(original, ['database-migrate'], admin=True)
        # Actual hard-killed generation/effect owners seed durable unknown
        # accounting; SQL snapshots later prove these are not resumed/reset.
        binary = root / 'operational-faults.exe'
        shutil.copyfile(args.fault_test.resolve(), binary)
        report['fault_test_sha256'] = dr.sha(binary.read_bytes())
        environment = dict(os.environ, TASKLOOM_TEST_POSTGRES_URL=json.loads((fixtures['a'] / 'credentials.json').read_bytes())['TASKLOOM_TEST_POSTGRES_URL'],
                           TASKLOOM_FAULT_OUTPUT=str(assets / 'external_effects'))
        with (root / 'private-operational-faults.log').open('xb') as stream:
            result = subprocess.run([str(binary), '--exact', 'postgres_operational_process_and_owner_faults', '--ignored', '--nocapture'],
                                    env=environment, stdout=stream, stderr=stream, timeout=180)
        check('actual_unknown_generation_effect_faults_seeded', result.returncode == 0)
        fault_report = json.loads((assets / 'external_effects' / 'postgres' / 'report.json').read_bytes())
        check('actual_unknown_fault_assertions', fault_report['status'] == 'passed' and all(fault_report['checks'].values()))
        catalog_before = a.fingerprint('taskloom_accept')
        a.sql("CREATE SCHEMA tl_dr_profile_extra; CREATE FUNCTION tl_dr_profile_extra.f() RETURNS int LANGUAGE sql AS 'SELECT 1';")
        try:
            check('nonpublic_function_profile_refused', refuse(lambda: a.fingerprint('taskloom_accept')))
        finally:
            a.sql('DROP SCHEMA tl_dr_profile_extra CASCADE;')
        a.sql('CREATE DOMAIN public.tl_dr_profile_domain AS text;')
        try:
            check('independent_custom_type_profile_refused', refuse(lambda: a.fingerprint('taskloom_accept')))
        finally:
            a.sql('DROP DOMAIN public.tl_dr_profile_domain;')
        check('profile_negative_cleanup_preserves_database', a.fingerprint('taskloom_accept') == catalog_before)
        original_role = 'tl_dr_initial_' + nonce[:16]
        password = uuid.uuid4().hex + uuid.uuid4().hex
        a.sql('CREATE ROLE ' + dr.identifier(original_role) + ' LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD ' + dr.literal(password) + '; '
              'REVOKE ALL ON DATABASE taskloom_accept FROM PUBLIC; GRANT CONNECT ON DATABASE taskloom_accept TO ' + dr.identifier(original_role) + ';')
        a.sql('GRANT USAGE ON SCHEMA public TO ' + dr.identifier(original_role) + '; GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA public TO ' + dr.identifier(original_role) + ';')
        original_url = 'postgres://' + original_role + ':' + password + '@a:5432/taskloom_accept?sslmode=disable'
        dr.private_save(assets / 'secrets' / 'deployment.json', {'role': original_role, 'database': 'taskloom_accept', 'url': original_url})
        state1 = controller.initialize('a', 'taskloom_accept', original_role, assets)
        dr.private_save(assets / 'config' / 'queue-limits.json', {'max_open_jobs': 8, 'max_running': 2})
        app(state1, ['queue-configure', '/managed/config/queue-limits.json', '0', 'dr-local-operator', 'offline managed DR finite fixture'])
        job1 = task(state1, 'source-job')['id']
        source_worker = controller.runtime(state1, 'worker', 'dr-source', worker=True)
        check('real_source_worker_completed', wait_completed(state1, job1)['state'] == 'completed')
        before = a.fingerprint(state1['database'])
        state2, receipt2 = controller.promote('b', root / 'promotion-a-b')
        check('epoch2_target_active_ready', state2['epoch'] == 2 and state2['region'] == 'b' and state2['ready'])
        check('source_role_permanently_nologin', not a.role(original_role)['rolcanlogin'])
        check('actual_source_worker_stopped', not json.loads(dr.docker('inspect', source_worker, '--format', '{{json .State}}'))['Running'])
        check('full_database_and_unknown_unchanged_after_restore', controller.regions['b'].fingerprint(state2['database']) == before)
        check('stale_witness_cas_refused', refuse(lambda: controller.cas(state1, dict(state1, ready=False), 'stale-test')))
        check('stale_managed_source_start_refused', refuse(lambda: controller.runtime(state1, 'worker', 'stale', worker=True)))
        check('old_source_credential_connection_refused', refuse(lambda: app(state1, ['queue'])))
        job2 = task(state2, 'target-job')['id']
        controller.runtime(state2, 'worker', 'dr-target', worker=True)
        check('real_target_worker_completed', wait_completed(state2, job2)['state'] == 'completed')
        # Reverse promotion uses a NEW database/role and epoch, never old role.
        before_reverse = controller.regions['b'].fingerprint(state2['database'])
        state3, receipt3 = controller.promote('a', root / 'promotion-b-a')
        check('rollback_is_new_epoch_and_new_role_database', state3['epoch'] == 3 and state3['role'] != original_role and state3['database'] != state1['database'])
        check('both_historical_roles_disabled', not a.role(original_role)['rolcanlogin'] and not controller.regions['b'].role(state2['role'])['rolcanlogin'])
        check('reverse_restore_preserves_full_database', a.fingerprint(state3['database']) == before_reverse)
        # Live source failure: inability to prove actual role/session fencing
        # must not promote, even though the target and witness are available.
        dr.docker('pause', a.cid)
        try:
            check('unreachable_source_refuses_promotion', refuse(lambda: dr.Controller(root / 'spec.json').promote('b', root / 'refuse-unreachable-source')))
        finally:
            dr.docker('unpause', a.cid)
        check('unreachable_preflight_keeps_authority_unchanged', controller.state() == state3)
        # Real restore/activation followed by an injected lost acknowledgement.
        # The operation becomes Unknown and managed startup stays denied.
        def lost_ack(stage, _state):
            if stage == 'active_lost_ack':
                raise TimeoutError('injected lost acknowledgement after committed Active')
        check('lost_active_acknowledgement_refused', refuse(lambda: controller.promote('b', root / 'lost-active-ack', failpoint=lost_ack)))
        unknown = controller.state()
        check('lost_ack_transition_unknown_no_target_start', unknown['status'] == 'unknown' and not unknown['ready'] and
              refuse(lambda: controller.runtime(unknown, 'worker', 'forbidden', worker=True)))
        check('unknown_never_reenables_historical_source', not a.role(original_role)['rolcanlogin'])
        audit = controller.witness.value('SELECT json_agg(json_build_object(\'epoch\',epoch,\'event\',event) ORDER BY sequence) FROM tl_dr_audit;')
        check('witness_monotonic_epoch_audit', [row['epoch'] for row in audit] == sorted(row['epoch'] for row in audit) and unknown['epoch'] == 4)
        report['promotions'] = [receipt2, receipt3]
        report['witness_audit'] = audit
        report['status'] = 'passed'
    except Exception as error:
        report['failures'].append(type(error).__name__ + ': local private evidence retained')
        raise
    finally:
        cleanup = []
        if controller:
            try:
                for item in controller.containers():
                    dr.docker('rm', '--force', item['Id'])
                cleanup.append('exact managed worker containers removed')
            except Exception:
                cleanup.append('worker cleanup failed; inspect exact deployment label locally')
        for path in fixtures.values():
            if (path / 'ownership.json').exists():
                try:
                    fixture('remove', path)
                except Exception:
                    cleanup.append('owned fixture cleanup failed: ' + path.name)
        if network:
            try:
                current = json.loads(dr.docker('network', 'inspect', network, '--format', '{{json .}}'))
                if current['Labels'].get(dr.LABEL) != nonce:
                    raise dr.Refused('network identity changed')
                dr.docker('network', 'rm', network)
                cleanup.append('exact owned network removed')
            except Exception:
                cleanup.append('owned network cleanup failed')
        report['cleanup'] = cleanup
        if any('failed' in value for value in cleanup):
            report['status'] = 'failed'
        dr.private_save(root / 'report.json', report)
    print(json.dumps({'status': report['status'], 'checks': report['checks'], 'cloud_model_requests': 0}))


if __name__ == '__main__':
    main()
