"""Quiescent SQLite online-backup / owned PostgreSQL dump-restore acceptance.

Uses the completed native SDK campaign, private local fixture credentials and
its owned container. Restores into NEW destinations only and cold-inspects them
without any model credential. Never publishes backups, URLs or passwords.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
from urllib.parse import quote,urlsplit,urlunsplit
import uuid


def save(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def docker(*args):return subprocess.check_output(['docker',*args],stderr=subprocess.PIPE,text=True,timeout=60).strip()
def digest(path):return {'bytes':path.stat().st_size,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path);parser.add_argument('--native',type=Path,required=True)
    parser.add_argument('--postgres-fixture',type=Path,required=True);parser.add_argument('--example',type=Path,required=True)
    args=parser.parse_args();native=args.native.resolve();campaign=json.loads((native/'report.json').read_text(encoding='utf-8'))
    if not campaign['completed'] or campaign['status']!='passed':raise ValueError('completed passing native SDK campaign required')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    cases={c['backend']:c for c in campaign['cases']};creds=json.loads((args.postgres_fixture/'credentials.json').read_text())
    ownership=json.loads((args.postgres_fixture/'ownership.json').read_text());cid=ownership['container_id']
    if not re.fullmatch('[0-9a-f]{64}',cid):raise ValueError('invalid owned container identity')
    state=json.loads(docker('inspect',cid,'--format','{{json .}}'))
    if state['Config']['Labels'].get('com.taskloom.database-acceptance')!=ownership['nonce'] or state['Image']!=ownership['image_id']:
        raise ValueError('fixture ownership/image changed')
    def inspect(url,backend):
        env=dict(os.environ,TASKLOOM_DATABASE_URL=url);env.pop(campaign['fees']['model_credential_environment'],None)
        result=subprocess.run([str(args.example.resolve()),'inspect','native-database',cases[backend]['project']],env=env,capture_output=True,timeout=30)
        if result.returncode:raise RuntimeError('cold SQL inspection failed; credential value omitted')
        return json.loads(result.stdout)
    report={'version':1,'completed':False,'checks':{},'failures':[],
        'scope':'quiescent core SQL run/workflow/session/budget tables only; no whole-app/file/knowledge/memory/skills backup or worker-service completion',
        'native_reference':str(native),'binary_sha256':hashlib.sha256(args.example.read_bytes()).hexdigest(),
        'model_requests_performed':False,'credential_values':'private fixture environment only; never included in output',
        'artifacts_private':'all database/dump files contain runtime data; excluded from source publication'}
    try:
        source=native/'runtime ü space.sqlite';target=root/'restored ü space.sqlite'
        with sqlite3.connect(source.as_uri()+'?mode=ro',uri=True) as original:
            live=original.execute("SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)").fetchone()[0]
            if live:raise ValueError('stop all SQLite owners before backup acceptance')
            target.open('xb').close()
            with sqlite3.connect(target) as restored:original.backup(restored)
        original_url='sqlite://'+quote(str(source),safe='')+'?mode=rw&cache=private'
        restored_url='sqlite://'+quote(str(target),safe='')+'?mode=rw&cache=private'
        a=inspect(original_url,'sqlite');b=inspect(restored_url,'sqlite')
        save(root/'sqlite-original-cold.json',a);save(root/'sqlite-restored-cold.json',b)
        report['checks']['sqlite_online_backup_restores_identical_native_state']=a==b
        report['sqlite_backup']=digest(target)
        live=docker('exec',cid,'psql','-U','taskloom_accept','-d','taskloom_accept','-Atc',
            'SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST(floor(EXTRACT(EPOCH FROM clock_timestamp())*1000) AS BIGINT)')
        if live!='0':raise ValueError('stop all PostgreSQL owners before backup acceptance')
        dump=root/'runtime.dump'
        with dump.open('xb') as stream:
            result=subprocess.run(['docker','exec',cid,'pg_dump','-U','taskloom_accept','-d','taskloom_accept','-Fc'],stdout=stream,stderr=subprocess.PIPE,timeout=60)
        if result.returncode:raise RuntimeError('owned pg_dump failed')
        restored_db='taskloom_restore_'+uuid.uuid4().hex[:16]
        docker('exec',cid,'createdb','-U','taskloom_accept','-O','taskloom_accept',restored_db)
        with dump.open('rb') as stream:
            result=subprocess.run(['docker','exec','--interactive',cid,'pg_restore','--exit-on-error','--single-transaction','--no-owner',
                '-U','taskloom_accept','-d',restored_db],stdin=stream,capture_output=True,timeout=60)
        if result.returncode:raise RuntimeError('owned pg_restore failed')
        original_pg=creds['TASKLOOM_TEST_POSTGRES_URL'];parsed=urlsplit(original_pg)
        restored_pg=urlunsplit((parsed.scheme,parsed.netloc,'/'+restored_db,parsed.query,parsed.fragment))
        a=inspect(original_pg,'postgres');b=inspect(restored_pg,'postgres')
        save(root/'postgres-original-cold.json',a);save(root/'postgres-restored-cold.json',b)
        report['checks']['postgres_transactional_restore_retains_identical_native_state']=a==b
        report['postgres_backup']=digest(dump);report['restored_owned_database_name']=restored_db
        report['checks']['unknown_model_attempts_retained_after_restore']=all(v['long_run']['report']['budget']['uncertain_attempts']==1 for v in [
            json.loads((root/'sqlite-restored-cold.json').read_text(encoding='utf-8')),b])
        report['completed']=True
    except Exception as error:report['failures'].append(str(error))
    report['failures'].extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
    report['status']='failed' if report['failures'] else 'passed';save(root/'report.json',report)
    print(json.dumps(report,ensure_ascii=False));raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
