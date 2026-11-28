"""Repeatable single-host SQL/process outage and quiescent restore drills.

Uses copied Taskloom binaries, actual SQLite/owned Docker PostgreSQL, an explicit
offline fixture model and real fsynced local effects. Never calls a cloud model.
This is not a multihost partition, rolling-schema, native quality or SLO test.
All outputs (including databases, bearer files and dumps) are private local data.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit, urlunsplit
from urllib.request import ProxyHandler, build_opener
import uuid


TABLES = {
    'taskloom_meta': ['singleton', 'version'],
    'taskloom_records': ['namespace', 'family', 'object_id', 'revision', 'body'],
    'taskloom_leases': ['namespace', 'family', 'object_id', 'owner', 'epoch', 'expires_ms'],
    'taskloom_jobs': ['namespace', 'job_id', 'resource', 'state', 'is_open', 'created_ms', 'revision'],
    'taskloom_job_bindings': ['namespace', 'family', 'object_id', 'job_id'],
}


def save(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write('\n')


def digest(path):
    return {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--admin', type=Path, required=True)
    parser.add_argument('--fault-test', type=Path, required=True,
                        help='compiled operational_faults integration-test executable')
    parser.add_argument('--postgres-fixture', type=Path, required=True,
                        help='owned postgres_fixture.py directory; never an arbitrary database URL')
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).resolve().parents[2]
    ownership = json.loads((args.postgres_fixture/'ownership.json').read_text())
    public = json.loads((args.postgres_fixture/'public.json').read_text())
    pg = json.loads((args.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    cid = ownership['container_id']
    if not re.fullmatch('[a-f0-9]{64}', cid):
        raise ValueError('invalid owned container identity')
    parsed = urlsplit(pg)
    if parsed.hostname != '127.0.0.1' or parsed.port != public['loopback_port']:
        raise ValueError('owned loopback PostgreSQL required')
    hidden = [pg, parsed.password]
    processes = []
    paused = False
    restored_db = None
    report = {'version': 1, 'completed': False, 'checks': {}, 'failures': [],
              'scope': __doc__, 'postgres_provenance': public,
              'cloud_model_requests': 0, 'model': 'explicit offline fixtures',
              'limits': [
                  'Single local host and disposable local PostgreSQL; database pause is not a distributed network partition.',
                  'SQLite write contention is not a network outage; readiness may remain readable in WAL mode.',
                  'Hard-killed owned child processes and explicitly injected SQL epochs do not prove multihost failover.',
                  'Quiescent restore is not rolling migration, live backup coordination or production RPO/RTO/SLO acceptance.',
                  'External fsynced effect files are outside database backups; restoring a database never authorizes replay.',
                  'No cloud/provider request; fixture load throughput does not measure native model effectiveness.',
                  'Windows service cleanup is explicit process termination; graceful SIGTERM is separately tested on POSIX.',
              ]}

    def clean(raw):
        value = raw.decode('utf-8', errors='replace') if isinstance(raw, bytes) else str(raw)
        for secret in hidden:
            if secret:
                value = value.replace(secret, '[private database credential]')
        return value

    def docker(*parts, timeout=60):
        result = subprocess.run(['docker', *parts], capture_output=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError('owned Docker operation failed: '+str(parts[0])+': '+clean(result.stderr)[:500])
        return result.stdout.decode('utf-8').strip()

    def verify_owner():
        state = json.loads(docker('inspect', cid, '--format', '{{json .}}'))
        if state['Config']['Labels'].get('com.taskloom.database-acceptance') != ownership['nonce'] or state['Image'] != ownership['image_id']:
            raise ValueError('fixture ownership/image changed; refuse mutation')
        return state

    def env(url):
        result = dict(os.environ, TASKLOOM_DATABASE_URL=url)
        # The selected configurations use offline providers. Child fixture tests
        # use only their own explicit Model implementation and local filesystem.
        return result

    def logged(name, parts, environment, timeout=180):
        result = subprocess.run(list(map(str, parts)), env=environment, capture_output=True, timeout=timeout)
        with (root/name).open('x', encoding='utf-8') as stream:
            stream.write(clean(result.stdout)+'\n'+clean(result.stderr))
        if result.returncode:
            raise RuntimeError(name+' failed with exit '+str(result.returncode)+'; see sanitized local log')
        return result

    def cli(backend, url, *parts):
        result = subprocess.run([str(exe), '--config', str(root/'load'/backend/'app.json'), *map(str, parts)],
                                env=env(url), capture_output=True, timeout=90)
        if result.returncode:
            raise RuntimeError('cold/fixture CLI '+str(parts[0])+' failed: '+clean(result.stderr)[:500])
        return json.loads(result.stdout)

    def snapshot(backend, database):
        values = {}
        for table, columns in TABLES.items():
            fields = ','.join(columns)
            if backend == 'sqlite':
                with sqlite3.connect(database.as_uri()+'?mode=ro', uri=True) as connection:
                    values[table] = [list(row) for row in connection.execute('SELECT '+fields+' FROM '+table+' ORDER BY '+fields)]
            else:
                sql = "SELECT coalesce(json_agg(json_build_array("+fields+") ORDER BY "+fields+"),'[]'::json)::text FROM "+table
                values[table] = json.loads(docker('exec', cid, 'psql', '-X', '-U', 'taskloom_accept', '-d', database, '-Atc', sql))
        return values

    def live_leases(backend, database):
        if backend == 'sqlite':
            with sqlite3.connect(database.as_uri()+'?mode=ro', uri=True) as connection:
                return connection.execute("SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)").fetchone()[0]
        return int(docker('exec', cid, 'psql', '-X', '-U', 'taskloom_accept', '-d', database, '-Atc',
                          'SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST(floor(EXTRACT(EPOCH FROM clock_timestamp())*1000) AS BIGINT)'))

    def check_unknown(values, backend):
        fixture = json.loads((root/'faults'/backend/'report.json').read_text(encoding='utf-8'))
        records = {(namespace, family, identity): json.loads(body)
                   for namespace, family, identity, revision, body in values['taskloom_records']}
        model = fixture['model_death']
        effect = fixture['pending_effect']
        def record(case, family, identity):
            namespace = json.dumps([case['tenant'], case['project']], separators=(',', ':'))
            envelope = records[(namespace, family, identity)]
            if (envelope['version'], envelope['namespace'], envelope['family'], envelope['id']) != (1, namespace, family, identity):
                raise ValueError('restored record envelope identity mismatch')
            return envelope['value']
        retained_model = record(model, 'run', 'model')
        retained_effect = record(effect, 'run', 'effect')
        return {
            'unknown_generation_attempt_preserved': retained_model == model['run'] and retained_model['report']['budget']['uncertain_attempts'] == 1,
            'unreconciled_effect_preserved_without_replay': retained_effect == effect['uncertain_run'] and retained_effect['status'] == 'needs_recovery' and retained_effect['tool_steps'][0]['progress']['type'] == 'uncertain' and retained_effect['tool_steps'][0]['attempts'] == 1,
        }

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)

    try:
        state = verify_owner()
        if not state['State']['Running'] or state['State']['Paused']:
            raise ValueError('owned fixture must be running and unpaused')
        (root/'bin').mkdir()
        binaries = {}
        for name, original in [('taskloom', args.taskloom), ('admin', args.admin), ('fault-test', args.fault_test)]:
            original = original.resolve()
            target = root/'bin'/(name+original.suffix)
            with original.open('rb') as incoming, target.open('xb') as outgoing:
                shutil.copyfileobj(incoming, outgoing)
            target.chmod(original.stat().st_mode)
            binaries[name] = target
        exe, admin, fault_test = [binaries[name] for name in ['taskloom', 'admin', 'fault-test']]
        report['binaries'] = {name: digest(path) for name, path in binaries.items()}
        report['sources'] = {name: digest(source/name) for name in [
            'Cargo.toml', 'Cargo.lock', 'tests/operational_faults.rs',
            'examples/operations/fault_drill.py', 'examples/database/queue_load.py',
            'examples/database/postgres_fixture.py']}
        manifest = {path.relative_to(source).as_posix(): digest(path)
                    for path in sorted((source/'src').rglob('*')) if path.is_file()}
        manifest.update(report['sources'])
        save(root/'source-manifest.json', manifest)
        report['source_manifest'] = dict(digest(root/'source-manifest.json'), files=len(manifest),
                                        scope='source files sampled at start; binary hashes recorded separately')
        logged('queue-load.log', [sys.executable, source/'examples/database/queue_load.py', root/'load',
                                 '--taskloom', exe, '--admin', admin, '--postgres-fixture', args.postgres_fixture.resolve()], os.environ, 900)
        load = json.loads((root/'load/report.json').read_text(encoding='utf-8'))
        report['checks']['finite_sql_backlog_restart_and_caps'] = load['status'] == 'passed' and load['completed']
        report['backlog'] = {case['backend']: {'checks': case['checks'], 'measurements': case['measurements']} for case in load['cases']}
        (root/'faults').mkdir()
        sqlite_file = root/'load/load.sqlite'
        sqlite_url = 'sqlite://'+quote(str(sqlite_file), safe='')+'?mode=rw&cache=private'
        for backend in ['sqlite', 'postgres']:
            environment = env(sqlite_url if backend == 'sqlite' else pg)
            environment.update(TASKLOOM_FAULT_OUTPUT=str(root/'faults'),
                               TASKLOOM_FAULT_SQLITE_URL=sqlite_url, TASKLOOM_TEST_POSTGRES_URL=pg)
            parts = [fault_test, '--exact', backend+'_operational_process_and_owner_faults', '--nocapture']
            if backend == 'postgres':
                parts.append('--ignored')
            logged(backend+'-process-faults.log', parts, environment, 180)
            faults = json.loads((root/'faults'/backend/'report.json').read_text(encoding='utf-8'))
            report['checks'][backend+'_process_death_unknown_accounting_and_fences'] = faults['status'] == 'passed' and all(faults['checks'].values())
            report[backend+'_process_checks'] = faults['checks']

        # Hold a genuine SQLite writer lock across a complete CLI admission
        # attempt. Snapshot equality plus the successful explicit retry prevents
        # a configuration failure from being mislabeled as a contention pass.
        before = snapshot('sqlite', sqlite_file)
        task = root/'sqlite-lock-task.json'
        save(task, {'type': 'run', 'id': 'write-lock-retry', 'prompt': 'explicit offline SQLite contention fixture'})
        with sqlite3.connect(sqlite_file, timeout=1) as blocker:
            blocker.execute('BEGIN IMMEDIATE')
            started = time.monotonic()
            refused = subprocess.run([str(exe), '--config', str(root/'load/sqlite/app.json'), 'queue-submit', str(task), 'drill-local-operator'],
                                     env=env(sqlite_url), capture_output=True, timeout=45)
            elapsed = round((time.monotonic()-started)*1000)
            blocker.rollback()
        after = snapshot('sqlite', sqlite_file)
        save(root/'sqlite-lock-refusal.json', {'exit_code': refused.returncode, 'elapsed_ms': elapsed, 'stderr': clean(refused.stderr)[:2000]})
        report['checks']['sqlite_lock_refuses_without_any_record_mutation'] = refused.returncode != 0 and elapsed >= 4000 and before == after
        submitted = cli('sqlite', sqlite_url, 'queue-submit', task, 'drill-local-operator')
        finished = cli('sqlite', sqlite_url, 'worker-once', 'after-lock-release')
        save(root/'sqlite-lock-explicit-retry.json', {'submitted': submitted, 'finished': finished})
        report['checks']['sqlite_explicit_retry_after_release_completes_once'] = finished['id'] == submitted['id'] and finished['state'] == 'completed' and finished['attempts'] == 1

        # An actual pause of the exact owned PostgreSQL container interrupts
        # existing DB requests while the real HTTP host stays live. No task is
        # submitted during this probe.
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        app = json.loads((root/'load/postgres/app.json').read_text())
        save(root/'outage-host.json', {'listen': '127.0.0.1:'+str(port), 'projects': [{
            'tenant': app['database']['tenant'], 'project': app['database']['project'],
            'config': str(root/'load/postgres/app.json')}]})
        opener = build_opener(ProxyHandler({}))
        def health(path):
            try:
                with opener.open('http://127.0.0.1:'+str(port)+path, timeout=8) as response:
                    return {'status': response.status, 'body': json.loads(response.read(8192))}
            except HTTPError as error:
                return {'status': error.code, 'body': json.loads(error.read(8192))}
        def wait_ready(process):
            for _ in range(150):
                if process.poll() is not None:
                    raise RuntimeError('owned host exited before readiness')
                try:
                    value = health('/health/ready')
                    if value['status'] == 200:
                        return value
                except (OSError, URLError):
                    pass
                time.sleep(.1)
            raise RuntimeError('owned host readiness deadline')
        before = snapshot('postgres', 'taskloom_accept')
        with (root/'outage-host.log').open('xb') as log:
            process = subprocess.Popen([str(exe), 'service-serve', str(root/'outage-host.json')], env=env(pg), stdout=log, stderr=log,
                                       creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            processes.append(process)
            healthy = wait_ready(process)
            verify_owner()
            docker('pause', cid)
            paused = True
            try:
                unavailable = health('/health/ready')
                live = health('/health/live')
            finally:
                verify_owner()
                docker('unpause', cid)
                paused = False
            recovered = wait_ready(process)
            stop(process)
        after = snapshot('postgres', 'taskloom_accept')
        save(root/'postgres-pause-health.json', {'before': healthy, 'paused_ready': unavailable, 'paused_live': live, 'after': recovered})
        report['checks']['postgres_pause_readiness_refuses_while_http_stays_live'] = unavailable['status'] == 503 and live['status'] == 200
        report['checks']['postgres_unpause_restores_readiness_without_record_mutation'] = recovered['status'] == 200 and before == after

        # The dump is only taken after every owned service/worker is stopped
        # and the database clock reports zero live leases. Expired leases are
        # retained in the canonical snapshot and compared too.
        for backend, database in [('sqlite', sqlite_file), ('postgres', 'taskloom_accept')]:
            for _ in range(120):
                if live_leases(backend, database) == 0:
                    break
                time.sleep(.1)
            else:
                raise ValueError('refuse backup with live '+backend+' owner leases')
        sqlite_original = snapshot('sqlite', sqlite_file)
        sqlite_restored = root/'restored.sqlite'
        sqlite_restored.open('xb').close()
        with sqlite3.connect(sqlite_file.as_uri()+'?mode=ro', uri=True) as original, sqlite3.connect(sqlite_restored) as restored:
            original.backup(restored)
        postgres_original = snapshot('postgres', 'taskloom_accept')
        verify_owner()
        dump = root/'postgres.dump'
        with dump.open('xb') as stream:
            result = subprocess.run(['docker', 'exec', cid, 'pg_dump', '-U', 'taskloom_accept', '-d', 'taskloom_accept', '-Fc'], stdout=stream, stderr=subprocess.PIPE, timeout=60)
        if result.returncode:
            raise RuntimeError('owned PostgreSQL dump failed')
        restored_db = 'taskloom_drill_restore_'+uuid.uuid4().hex[:16]
        docker('exec', cid, 'createdb', '-U', 'taskloom_accept', '-O', 'taskloom_accept', restored_db)
        with dump.open('rb') as stream:
            result = subprocess.run(['docker', 'exec', '--interactive', cid, 'pg_restore', '--exit-on-error', '--single-transaction', '--no-owner', '-U', 'taskloom_accept', '-d', restored_db], stdin=stream, capture_output=True, timeout=60)
        if result.returncode:
            raise RuntimeError('owned PostgreSQL restore failed')
        restored_pg = urlunsplit((parsed.scheme, parsed.netloc, '/'+restored_db, parsed.query, parsed.fragment))
        for backend, original, restored, original_url, restored_url in [
            ('sqlite', sqlite_original, snapshot('sqlite', sqlite_restored), sqlite_url, 'sqlite://'+quote(str(sqlite_restored), safe='')+'?mode=rw&cache=private'),
            ('postgres', postgres_original, snapshot('postgres', restored_db), pg, restored_pg)]:
            save(root/(backend+'-original-records.json'), original)
            save(root/(backend+'-restored-records.json'), restored)
            report['checks'][backend+'_restore_preserves_every_sql_table'] = original == restored
            for name, passed in check_unknown(restored, backend).items():
                report['checks'][backend+'_'+name] = passed
            a, b = cli(backend, original_url, 'queue'), cli(backend, restored_url, 'queue')
            save(root/(backend+'-restored-cold-queue.json'), b)
            report['checks'][backend+'_cold_queue_state_identical_after_restore'] = a == b
            case = json.loads((root/'faults'/backend/'report.json').read_text())['model_death']
            result = subprocess.run([str(admin), 'quota-get', case['tenant'], case['project']], env=env(restored_url), capture_output=True, timeout=30)
            if result.returncode:
                raise RuntimeError('restored unknown generation quota cold inspection failed')
            quota = json.loads(result.stdout)
            save(root/(backend+'-restored-unknown-quota.json'), quota)
            report['checks'][backend+'_cold_unknown_quota_matches_before_restore'] = quota == case['quota'] and quota['usage']['uncertain_attempts'] == 1
            effect = root/'faults'/backend/'pending-effect/actual-effect.jsonl'
            report['checks'][backend+'_external_effect_still_single_after_cold_inspection'] = len(effect.read_text().splitlines()) == 1
        report['backup_files'] = {'sqlite': digest(sqlite_restored), 'postgres': digest(dump)}
        report['completed'] = True
    except Exception as error:
        report['failures'].append(clean(error))
    finally:
        for process in processes:
            try:
                stop(process)
            except Exception as error:
                report['failures'].append('owned service cleanup failed: '+clean(error))
        if paused:
            try:
                verify_owner()
                docker('unpause', cid)
            except Exception as error:
                report['failures'].append('owned database unpause failed: '+clean(error))
        if restored_db:
            try:
                verify_owner()
                docker('exec', cid, 'dropdb', '-U', 'taskloom_accept', restored_db)
                report['restored_owned_database_removed'] = True
            except Exception as error:
                report['failures'].append('owned restore database cleanup failed: '+clean(error))
        report['failures'].extend('check failed: '+name for name, passed in report['checks'].items() if not passed)
        report['status'] = 'passed' if report['completed'] and not report['failures'] else 'failed'
        report['logs'] = {path.name: digest(path) for path in root.glob('*.log')}
        save(root/'report.json', report)
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if report['status'] == 'passed' else 1)


if __name__ == '__main__':
    main()
