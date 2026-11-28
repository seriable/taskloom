"""Upgrade copies of retained real schema-v1/v2 native databases to v3.

Source files remain untouched. Requires an explicitly owned local PostgreSQL
fixture and a previous native v1 dump; no model credentials or API calls.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import time
from urllib.parse import quote, urlsplit, urlunsplit
import uuid

from backup_acceptance import digest, docker, save


def expected_usage(rows, scope):
    namespace = json.dumps(scope, separators=(',', ':'))
    if not any(row['namespace'] == namespace and row['family'] == 'run' for row in rows):
        raise ValueError('retained v1 native runs required')
    latest = {}
    for row in rows:
        if row['namespace'] != namespace or row['family'] not in {'run', 'budget'}:
            continue
        value = json.loads(row['body'])['value']
        budget = value['report']['budget'] if row['family'] == 'run' else value
        previous = latest.get(row['object_id'])
        if previous and previous['generation'] == budget['generation'] and previous != budget:
            raise ValueError('retained input has conflicting generations')
        if previous is None or previous['generation'] < budget['generation']:
            latest[row['object_id']] = budget
    fields = ['committed_micros', 'in_flight_micros', 'uncertain_micros', 'in_flight_attempts', 'uncertain_attempts']
    return json.loads(namespace), dict(runs=len(latest), **{field: sum(budget[field] for budget in latest.values()) for field in fields})


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('out', type=Path)
    p.add_argument('--sqlite-v1', type=Path, required=True)
    p.add_argument('--postgres-v1-dump', type=Path, required=True)
    p.add_argument('--postgres-fixture', type=Path, required=True)
    p.add_argument('--native-campaign', type=Path, required=True, help='matching passing SQL native campaign with per-backend app.json scope')
    p.add_argument('--admin', type=Path, required=True)
    p.add_argument('--source-version',type=int,choices=[1,2],default=1)
    p.add_argument('--old-probe',type=Path,help='optional retained v2 inspector; source-version 2 only')
    p.add_argument('--old-owner',type=Path,help='optional v2 SDK lease-only process; source-version 2 only')
    a = p.parse_args()
    campaign = json.loads((a.native_campaign / 'report.json').read_text(encoding='utf-8'))
    if not campaign['completed'] or campaign['status'] != 'passed':
        raise ValueError('matching passing native campaign required')
    root = a.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    ownership = json.loads((a.postgres_fixture / 'ownership.json').read_text())
    cid = ownership['container_id']
    if not re.fullmatch('[0-9a-f]{64}', cid):
        raise ValueError('invalid owned database identity')
    container = json.loads(docker('inspect', cid, '--format', '{{json .}}'))
    if container['Config']['Labels'].get('com.taskloom.database-acceptance') != ownership['nonce'] or container['Image'] != ownership['image_id']:
        raise ValueError('owned database identity changed')
    url = json.loads((a.postgres_fixture / 'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    report = {'version': 1, 'completed': False, 'checks': {}, 'failures': [], 'cases': [],
              'scope': f'actual retained native schema-v{a.source_version} SQLite/PostgreSQL copies, v3 migration and quota import/preservation',
              'model_api_calls': False, 'admin_binary': digest(a.admin),
              'v1_inputs': {'sqlite': digest(a.sqlite_v1), 'postgres_dump': digest(a.postgres_v1_dump)},
              'limits': ['Copies only; original acceptance databases/dumps remain untouched.',
                         'No old-binary dispatch or production rolling-upgrade/load acceptance; old hosts must be stopped.',
                         'Runtime data and credential URLs remain private ignored local artifacts.']}
    try:
        source = a.sqlite_v1.resolve()
        destination = root / 'migrated.sqlite'
        destination.open('xb').close()
        with sqlite3.connect(source.as_uri() + '?mode=ro', uri=True) as old, sqlite3.connect(destination) as new:
            if old.execute('SELECT version FROM taskloom_meta').fetchone()[0] != a.source_version:
                raise ValueError('retained SQLite input schema differs from requested source version')
            if old.execute("SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)").fetchone()[0]:
                raise ValueError('retained SQLite has live owners')
            old.backup(new)
        restored_db = 'quota_v1_' + uuid.uuid4().hex[:16]
        docker('exec', cid, 'createdb', '-U', 'taskloom_accept', '-O', 'taskloom_accept', restored_db)
        with a.postgres_v1_dump.open('rb') as stream:
            result = subprocess.run(['docker', 'exec', '--interactive', cid, 'pg_restore', '--exit-on-error', '--single-transaction', '--no-owner', '-U', 'taskloom_accept', '-d', restored_db], stdin=stream, capture_output=True, timeout=60)
        if result.returncode:
            raise RuntimeError('owned v1 PostgreSQL restore failed')
        parsed = urlsplit(url)
        postgres = urlunsplit((parsed.scheme, parsed.netloc, '/' + restored_db, parsed.query, parsed.fragment))

        def snapshot(backend):
            if backend == 'sqlite':
                with sqlite3.connect(destination) as connection:
                    connection.row_factory = sqlite3.Row
                    return {'version': connection.execute('SELECT version FROM taskloom_meta').fetchone()[0],
                            'records': [dict(row) for row in connection.execute('SELECT namespace,family,object_id,revision,body FROM taskloom_records ORDER BY namespace,family,object_id')]}
            query = "SELECT json_build_object('version',(SELECT version FROM taskloom_meta),'records',(SELECT json_agg(r) FROM (SELECT namespace,family,object_id,revision,body FROM taskloom_records ORDER BY namespace,family,object_id) r))"
            return json.loads(docker('exec', cid, 'psql', '-U', 'taskloom_accept', '-d', restored_db, '-Atc', query))

        for backend, database in {'sqlite': 'sqlite://' + quote(str(destination), safe='') + '?mode=rw&cache=private', 'postgres': postgres}.items():
            env = dict(os.environ, TASKLOOM_DATABASE_URL=database)
            before = snapshot(backend)
            save(root / (backend + '-before.json'), before)
            if before['version'] != a.source_version:
                raise ValueError('retained input schema differs from requested source version')
            probe=None;lease_owner=None;owner_log=None
            if a.old_probe:
                if a.source_version!=2:raise ValueError('old v2 binary controls require actual schema-v2 input')
                probe=[str(a.old_probe.resolve()),'inspect','legacy-cold-probe','unseen']
                if subprocess.run(probe,env=env,capture_output=True,timeout=30).returncode:raise RuntimeError('old v2 binary could not inspect the actual retained v2 copy')
                report['checks'][backend+'_old_v2_binary_read_before_upgrade']=True
                report['old_probe_binary']=digest(a.old_probe)
            if a.old_owner:
                if a.source_version!=2:raise ValueError('old v2 owner requires actual schema-v2 input')
                owner_log=(root/(backend+'-old-owner.log')).open('wb')
                lease_owner=subprocess.Popen([str(a.old_owner.resolve())],env=env,stdout=owner_log,stderr=owner_log,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
                try:
                    for _ in range(100):
                        if b'old-v2-owner-ready' in (root/(backend+'-old-owner.log')).read_bytes():break
                        if lease_owner.poll() is not None:raise RuntimeError('old v2 SDK lease process exited')
                        time.sleep(0.05)
                    else:raise RuntimeError('old v2 owner readiness deadline')
                    refusal=subprocess.run([str(a.admin.resolve()),'migrate'],env=env,capture_output=True,timeout=30)
                    report['checks'][backend+'_actual_old_v2_live_owner_blocks_upgrade']=refusal.returncode!=0 and b'quiescent' in refusal.stderr
                finally:
                    if lease_owner.poll() is None:lease_owner.kill();lease_owner.wait(timeout=10)
                    owner_log.close()
                report['old_owner_binary']=digest(a.old_owner)
                time.sleep(10.2)
            result = subprocess.run([str(a.admin.resolve()), 'migrate'], env=env, capture_output=True, timeout=30)
            if result.returncode:
                raise RuntimeError('copied native v1 migration failed')
            after = snapshot(backend)
            save(root / (backend + '-after.json'), after)
            report['checks'][backend + '_v3_barrier_preserves_every_existing_record'] = after['version'] == 3 and before['records'] == after['records']
            if probe:
                refused=subprocess.run(probe,env=env,capture_output=True,timeout=30)
                report['checks'][backend+'_old_v2_binary_refuses_upgraded_schema']=refused.returncode!=0 and b'database schema' in refused.stderr
            config = json.loads((a.native_campaign / backend / 'app.json').read_text(encoding='utf-8'))
            if not config.get('prices') or any(price['input_per_million'] or price['output_per_million'] for price in config['prices']):
                raise ValueError('retained native scope requires explicit zero-priced model configuration')
            scope, expected = expected_usage(before['records'], [config['database']['tenant'],config['database']['project']])
            policy = root / (backend + '-policy.json')
            save(policy, {'currency': 'USD', 'max_cost_micros': expected['committed_micros'] + expected['in_flight_micros'],
                          'max_outstanding_attempts': expected['in_flight_attempts'] + expected['uncertain_attempts']})
            command=[str(a.admin.resolve()),'quota-get',*scope] if a.source_version==2 else [str(a.admin.resolve()), 'quota-set', *scope, str(policy), '0', 'migration-operator', 'import retained actual native zero-priced USD accounting without raising capacity']
            result = subprocess.run(command, env=env, capture_output=True, timeout=30)
            if result.returncode:
                raise RuntimeError('first quota import from retained native v1 failed')
            imported = json.loads(result.stdout)
            save(root / (backend + '-quota.json'), imported)
            report['checks'][backend + '_quota_import_matches_newest_native_budgets'] = imported['usage'] == expected
            report['cases'].append({'backend': backend, 'retained_records': len(before['records']), 'imported_usage': imported['usage']})
        report['completed'] = True
    except Exception as error:
        report['failures'].append(str(error))
    report['failures'].extend('check failed: ' + key for key, passed in report['checks'].items() if not passed)
    report['status'] = 'failed' if report['failures'] else 'passed'
    save(root / 'report.json', report)
    print(json.dumps(report))
    raise SystemExit(1 if report['failures'] else 0)


if __name__ == '__main__':
    main()
