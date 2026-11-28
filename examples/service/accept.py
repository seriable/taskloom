"""Real separate-process SQL-identity / loopback service acceptance.

Applications use file stores and the explicitly offline demonstration model.
No native inference, generation-quality/cost benchmark or durable worker/queue
completion. Generated bearer files are private ignored runtime data only.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import Request, urlopen
import uuid


def save(path,value):
    path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--example',type=Path,required=True)
    parser.add_argument('--postgres-fixture',type=Path,required=True)
    parser.add_argument('--sql-apps',action='store_true',help='select SQL for project Application state as well as identities')
    parser.add_argument('--sqlite-database',type=Path,help='explicit existing owned SQLite acceptance database; adds fresh isolated test projects')
    args=parser.parse_args()
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    example=args.example.resolve()
    public=json.loads((args.postgres_fixture/'public.json').read_text())
    pg=json.loads((args.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    endpoint=urlsplit(pg)
    if endpoint.hostname!='127.0.0.1' or endpoint.port!=public['loopback_port']:
        raise ValueError('owned loopback database required')
    report={'version':1,'completed':False,'cases':[],'failures':[],
        'scope':'separate-process API authorization/revocation/restart, SQL identities and '+('SQL' if args.sql_apps else 'file')+' project journals, offline fixture model',
        'binary_sha256':hashlib.sha256(example.read_bytes()).hexdigest(),'postgres_public_provenance':public,
        'model_provenance':'explicit offline demo, never native API effectiveness, cost or latency evidence',
        'limits':[('Project Applications explicitly use SQL in this campaign.' if args.sql_apps else 'Project Applications still use file journals in this campaign.')+' No durable queue/worker/quota acceptance.',
            'Project members share run/workflow/session history; user-private memory endpoints are disabled.',
            'Bearer files and database credential values stay private; the acceptance artifact excludes them.',
            'Revocation affects new requests; it does not retroactively cancel already-authorized active operations.',
            'Loopback HTTP is the tested transport; public TLS/reverse proxy/distributed deployment/load acceptance remains open.']}
    started=time.monotonic()
    sqlite_path=args.sqlite_database.resolve() if args.sqlite_database else root/'identities.sqlite'
    for backend,url in {'sqlite':'sqlite://'+quote(str(sqlite_path),safe='')+'?mode=rwc&cache=private','postgres':pg}.items():
        out=root/backend;out.mkdir();env=dict(os.environ,TASKLOOM_DATABASE_URL=url)
        result={'backend':backend,'checks':{},'failures':[]};process=None;log=None
        project='service-'+uuid.uuid4().hex
        users={role:role+'-'+uuid.uuid4().hex for role in ['runner','reader','operator']}
        tokens={};ids={}
        def command(*parts):
            value=subprocess.run([str(example),*map(str,parts)],env=env,capture_output=True,timeout=60)
            if value.returncode:raise RuntimeError('example command failed: '+value.stderr.decode(errors='replace')[:1200])
            return value.stdout.decode('utf-8')
        def catalog():return json.loads(command('catalog'))
        def call(path,role=None,method='GET',body=None):
            headers={}
            if role:headers['Authorization']='Bearer '+tokens[role]
            data=None if body is None else json.dumps(body).encode()
            if data is not None:headers['Content-Type']='application/json'
            request=Request(base+path,data=data,headers=headers,method=method)
            try:
                with urlopen(request,timeout=10) as response:return response.status,json.loads(response.read())
            except HTTPError as error:
                data=error.read()
                return error.code,json.loads(data) if data else None
        def start():
            nonlocal process,log
            log=(out/'server.log').open('ab')
            process=subprocess.Popen([str(example),'serve',str(out/'host.json')],env=env,stdout=log,stderr=log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
            for _ in range(100):
                if process.poll() is not None:raise RuntimeError('owned service exited during startup; see sanitized server log')
                try:
                    status,_=call(prefix+'/identity','operator')
                    if status==200:return
                except URLError:pass
                time.sleep(0.05)
            raise RuntimeError('owned loopback service readiness deadline reached')
        def stop():
            nonlocal process,log
            if process and process.poll() is None:process.kill();process.wait(timeout=10)
            if log:log.close()
            process=None;log=None
        try:
            command('migrate')
            for role,user in users.items():
                path=out/(role+'.json');account={'id':user,'enabled':True,'memberships':[{'tenant':'team-a','project':project,'role':role}]}
                save(path,account);command('put-user',path,catalog()['revision'],'local-bootstrap')
                token_file=out/(role+'.private-token')
                receipt=json.loads(command('issue',user,token_file,'local-bootstrap'))
                tokens[role]=token_file.read_text();ids[role]=receipt['token_id']
            with socket.socket() as probe:probe.bind(('127.0.0.1',0));port=probe.getsockname()[1]
            base=f'http://127.0.0.1:{port}'
            prefix=f'/tenants/team-a/projects/{project}/api'
            for suffix in ['a','b']:
                config=json.loads((Path(__file__).resolve().parent/('project-'+suffix+'.json')).read_text())
                config['state_dir']=str(out/('state-'+suffix))
                if args.sql_apps:config['database']={'url_env':'TASKLOOM_DATABASE_URL','tenant':'team-'+suffix,'project':project}
                save(out/('app-'+suffix+'.json'),config)
            save(out/'host.json',{'listen':f'127.0.0.1:{port}','projects':[
                {'tenant':'team-a','project':project,'config':'app-a.json'},
                {'tenant':'team-b','project':project,'config':'app-b.json'}]})
            start();checks=result['checks']
            checks['missing_credential_is_unauthorized']=call(prefix+'/runs')[0]==401
            checks['reader_cannot_submit']=call(prefix+'/runs','reader','POST',{'id':'forbidden','prompt':'demo'})[0]==403
            checks['runner_cannot_change_scope']=call(f'/tenants/team-b/projects/{project}/api/runs','runner')[0]==403
            checks['root_single_operator_api_absent']=call('/api/runs','operator')[0]==404
            checks['runner_can_submit_project_run']=call(prefix+'/runs','runner','POST',{'id':'short','prompt':'Acknowledge the task.'})[0]==202
            for _ in range(100):
                status,snapshot=call(prefix+'/runs/short','reader')
                if status==200 and not snapshot['job']['active']:break
                time.sleep(0.05)
            else:raise RuntimeError('offline run did not reach idle')
            checks['actual_application_journal_reaches_completed']=snapshot['state']['report']['stop_reason']=='completed'
            if args.sql_apps:checks['sql_mode_does_not_create_file_run_journals']=not (out/'state-a/runs').exists() and not (out/'state-b/runs').exists()
            save(out/'completed.json',snapshot)
            account=json.loads((out/'runner.json').read_text());account['memberships'][0]['role']='reader';save(out/'downgrade.json',account)
            command('put-user',out/'downgrade.json',catalog()['revision'],'local-admin')
            checks['separate_admin_process_downgrade_takes_effect_without_server_restart']=call(prefix+'/runs','runner','POST',{'id':'downgraded','prompt':'demo'})[0]==403
            command('revoke',ids['runner'],'local-admin')
            checks['separate_admin_process_revocation_takes_effect_without_server_restart']=call(prefix+'/identity','runner')[0]==401
            current=catalog();save(out/'catalog-public-metadata.json',current)
            checks['catalog_contains_only_token_digests']=all(value not in json.dumps(current) for value in tokens.values())
            stop();start()
            status,restarted=call(prefix+'/runs/short','reader');save(out/'restarted.json',restarted)
            checks['host_restart_keeps_application_state_and_sql_revocation']=status==200 and restarted==snapshot and call(prefix+'/identity','runner')[0]==401
            result['model_kind']='offline fixture; no native model API requests'
        except Exception as error:result['failures'].append(str(error))
        finally:stop()
        result['failures'].extend('check failed: '+name for name,passed in result['checks'].items() if not passed)
        result['status']='failed' if result['failures'] else 'passed';save(out/'report.json',result)
        report['cases'].append(result);report['failures'].extend(backend+': '+x for x in result['failures'])
    report['completed']=True;report['wall_ms']=round((time.monotonic()-started)*1000)
    report['status']='failed' if report['failures'] else 'passed';save(root/'report.json',report)
    print(json.dumps(report,ensure_ascii=False));raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
