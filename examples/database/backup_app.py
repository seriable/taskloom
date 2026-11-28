"""Quiescent full SQL Application state backup / cold restore acceptance.

Native campaigns and optional learned-embedding configs are inspected without
model/embedding credentials. Config files, environment/secret delivery, model
weights and external effects are separate deployment resources, not this backup.
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

from backup_acceptance import save,digest,docker

def persisted_snapshot(value):
    # CLI projection capture time is generated on each read; every persisted
    # event/span clock, revision, content and identity must still match exactly.
    value=json.loads(json.dumps(value))
    for trace in value['diagnostics'].values():trace['snapshot'].pop('captured_unix_ms')
    return value


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path);parser.add_argument('--native',type=Path,required=True)
    parser.add_argument('--taskloom',type=Path,required=True);parser.add_argument('--admin',type=Path,required=True)
    parser.add_argument('--postgres-fixture',type=Path,required=True)
    parser.add_argument('--embedding-sqlite',type=Path);parser.add_argument('--embedding-postgres',type=Path)
    args=parser.parse_args();native=args.native.resolve();campaign=json.loads((native/'report.json').read_text(encoding='utf-8'))
    if campaign['status']!='passed' or not campaign['completed']:raise ValueError('completed passing native SQL Application campaign required')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    ownership=json.loads((args.postgres_fixture/'ownership.json').read_text());cid=ownership['container_id']
    if not re.fullmatch('[0-9a-f]{64}',cid):raise ValueError('invalid owned database container identity')
    container=json.loads(docker('inspect',cid,'--format','{{json .}}'))
    if container['Config']['Labels'].get('com.taskloom.database-acceptance')!=ownership['nonce'] or container['Image']!=ownership['image_id']:
        raise ValueError('owned database identity changed')
    pg=json.loads((args.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    example=args.taskloom.resolve();admin=args.admin.resolve()
    def snapshot(url,backend):
        config=native/backend/'app.json';cfg=json.loads(config.read_text(encoding='utf-8'))
        env=dict(os.environ,TASKLOOM_DATABASE_URL=url);env.pop(cfg['provider']['key_env'],None)
        def cli(parts,config=config):
            value=subprocess.run([str(example),'--config',str(config),*parts],capture_output=True,env=env,timeout=60)
            if value.returncode:raise RuntimeError('cold SQL snapshot '+parts[0]+' failed; no credential values logged')
            return json.loads(value.stdout)
        listing=cli(['list']);skills=cli(['skills'])
        value={'runs':{r['id']:cli(['inspect',r['id']]) for r in listing},'memory':cli(['memory']),
            'knowledge':cli(['knowledge']),'skills':skills,'skill_objects':{},'diagnostics':{},'retrieval':cli(['index-status'])}
        for key,entry in skills['entries'].items():
            pin=root/(backend+'-'+entry['reference']['digest']+'.pin.json');save(pin,entry['reference'])
            value['skill_objects'][key]=cli(['skill-inspect',str(pin)])
        for kind,identity in [('workflow','flow'),('workflow','template'),('session','source-session'),('session','memory-session')]:
            value['diagnostics'][kind+'/'+identity]=cli(['diagnostics',kind,identity])
        identities=subprocess.run([str(admin),'catalog'],env=env,capture_output=True,timeout=30)
        if identities.returncode:raise RuntimeError('cold identity catalog inspection failed')
        value['identities']=json.loads(identities.stdout)
        extra=args.embedding_sqlite if backend=='sqlite' else args.embedding_postgres
        if extra:
            embedding=json.loads(extra.read_text(encoding='utf-8'));provider=embedding['retrieval']['embedding']['provider']
            env.pop(provider['key_env'],None)
            value['learned_embedding']=cli(['index-status'],extra.resolve())
        return value
    report={'version':1,'completed':False,'checks':{},'failures':[],
        'scope':'quiescent SQL run/workflow/session/budget/corpus/memory/skills/embedding/identity data plus dataset identity; no external files/secret/config/weights/effects backup',
        'binary_sha256':hashlib.sha256(example.read_bytes()).hexdigest(),'model_api_calls_performed':False,
        'credential_values':'private environment only; database/dump/bearer files excluded from source',
        'comparison_exclusions':['diagnostics/*/snapshot/captured_unix_ms only; newly generated observation clock, not persisted event time. Raw snapshots retained.'],
        'limits':['Stop admission/workers before backup; cold inspection verifies restored data without executing work.',
            'Dataset identity, approved-operation journals and revocation metadata are preserved. A restored backup is not authority to replay external effects.',
            'Protect separate host configs/secrets and review/rotate restored credentials before service admission. Production orchestration/queue/load acceptance remains open.']}
    try:
        source=native/'application ü.sqlite';target=root/'restored ü.sqlite'
        with sqlite3.connect(source.as_uri()+'?mode=ro',uri=True) as original:
            live=original.execute("SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)").fetchone()[0]
            if live:raise ValueError('SQLite owners remain live; stop admission/workers first')
            target.open('xb').close()
            with sqlite3.connect(target) as restored:original.backup(restored)
        old='sqlite://'+quote(str(source),safe='')+'?mode=rw&cache=private';new='sqlite://'+quote(str(target),safe='')+'?mode=rw&cache=private'
        a=snapshot(old,'sqlite');b=snapshot(new,'sqlite');save(root/'sqlite-original.json',a);save(root/'sqlite-restored.json',b)
        report['checks']['sqlite_native_application_persisted_snapshots_equal']=persisted_snapshot(a)==persisted_snapshot(b);report['sqlite_backup']=digest(target)
        live=docker('exec',cid,'psql','-U','taskloom_accept','-d','taskloom_accept','-Atc',
            'SELECT count(*) FROM taskloom_leases WHERE expires_ms>CAST(floor(EXTRACT(EPOCH FROM clock_timestamp())*1000) AS BIGINT)')
        if live!='0':raise ValueError('PostgreSQL owners remain live; stop admission/workers first')
        dump=root/'application.dump'
        with dump.open('xb') as stream:
            result=subprocess.run(['docker','exec',cid,'pg_dump','-U','taskloom_accept','-d','taskloom_accept','-Fc'],stdout=stream,stderr=subprocess.PIPE,timeout=60)
        if result.returncode:raise RuntimeError('owned PostgreSQL dump failed')
        destination='taskloom_app_restore_'+uuid.uuid4().hex[:16];docker('exec',cid,'createdb','-U','taskloom_accept','-O','taskloom_accept',destination)
        with dump.open('rb') as stream:
            result=subprocess.run(['docker','exec','--interactive',cid,'pg_restore','--exit-on-error','--single-transaction','--no-owner','-U','taskloom_accept','-d',destination],stdin=stream,capture_output=True,timeout=60)
        if result.returncode:raise RuntimeError('owned PostgreSQL restore failed')
        parsed=urlsplit(pg);restored_pg=urlunsplit((parsed.scheme,parsed.netloc,'/'+destination,parsed.query,parsed.fragment))
        a=snapshot(pg,'postgres');b=snapshot(restored_pg,'postgres');save(root/'postgres-original.json',a);save(root/'postgres-restored.json',b)
        report['checks']['postgres_native_application_persisted_snapshots_equal']=persisted_snapshot(a)==persisted_snapshot(b);report['postgres_backup']=digest(dump)
        report['restored_owned_database']=destination
        restored=[json.loads((root/'sqlite-restored.json').read_text(encoding='utf-8')),b]
        report['checks']['actual_identity_revocations_and_memberships_retained']=all(v['identities']['users'] and any(t['revoked'] for t in v['identities']['tokens'].values()) for v in restored)
        if args.embedding_sqlite and args.embedding_postgres:
            report['checks']['actual_learned_indexes_and_spending_valid_after_restore']=all(v['learned_embedding']['index_valid'] and v['learned_embedding']['embedding_spend']['requests']>0 for v in restored)
        report['completed']=True
    except Exception as error:report['failures'].append(str(error))
    report['failures'].extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
    report['status']='failed' if report['failures'] else 'passed';save(root/'report.json',report)
    print(json.dumps(report,ensure_ascii=False));raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
