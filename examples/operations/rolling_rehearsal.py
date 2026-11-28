"""Strict local same-schema worker replacement/rollback with two real binaries.

POSIX only. Old 0ae4bca supports graceful SIGINT, current supports SIGTERM.
Known responses finish before each worker is signaled; unknown attempts/effects
remain in separate namespaces and are cold-read only. No schema upgrade, cloud
model, live deployment, arbitrary tool replay or production SLO is implied.
"""
import argparse
from collections import Counter
from contextlib import contextmanager
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import threading
import time
from urllib.error import URLError
from urllib.parse import quote, urlsplit, urlunsplit
from urllib.request import ProxyHandler, Request, build_opener
import uuid

from binary_witness import source_files

OLD_COMMIT='0ae4bca0b5e85ca052f419a8fe0de0560c23fac3'
TABLES={
    'taskloom_meta':['singleton','version'],
    'taskloom_records':['namespace','family','object_id','revision','body'],
    'taskloom_leases':['namespace','family','object_id','owner','epoch','expires_ms'],
    'taskloom_jobs':['namespace','job_id','resource','state','is_open','created_ms','revision'],
    'taskloom_job_bindings':['namespace','family','object_id','job_id'],
}


def sha(path):
    value=hashlib.sha256()
    with path.open('rb') as stream:
        while chunk:=stream.read(1024*1024):value.update(chunk)
    return value.hexdigest()


def canonical(value):return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
def save(path,value):
    with path.open('x',encoding='utf-8') as stream:json.dump(value,stream,indent=2,ensure_ascii=False);stream.write('\n')


def verify_sources(old_path,old_hash,new_path,new_hash):
    """Pure admission checks; never launches a binary or touches a database."""
    if sha(old_path)!=old_hash or sha(new_path)!=new_hash:raise ValueError('source/build witness file differs from explicit approved SHA')
    old=json.loads(old_path.read_text());new=json.loads(new_path.read_text())
    if old.get('status')!='passed' or not old.get('completed') or old.get('source_commit')!=OLD_COMMIT or old.get('features')!=['app','database']:
        raise ValueError('old archive source/capability witness is not the approved exact release')
    if sha(old_path.parent/'source.tar')!=old['archive_sha256'] or sha(old_path.parent/'source/Cargo.lock')!=old['cargo_lock_sha256']:
        raise ValueError('old archive/lock source changed; refuse rollout')
    if new.get('kind')!='host_frozen_source_build_witness' or new.get('status')!='passed' or not new.get('completed') or new.get('features')!='all-features':
        raise ValueError('new source/capability witness is not the explicit frozen all-feature build')
    files=source_files(Path(new['source_root']))
    if files!=new['source_files'] or canonical(files)!=new['source_sha256']:raise ValueError('new frozen source changed; refuse rollout')
    if old.get('schema_version')!=3 or new.get('schema_version')!=3:raise ValueError('only explicitly shared schema3 is admitted; no migration')
    if old.get('rustc')!=new.get('rustc'):raise ValueError('old/new compiler witnesses differ')
    for descriptor in [old,new]:
        binary=Path(descriptor['binary']['path'])
        if sha(binary)!=descriptor['binary']['sha256']:raise ValueError('actual executable differs from its approved witness')
        with binary.open('rb') as stream:
            if stream.read(4)!=b'\x7fELF':raise ValueError('real POSIX ELF binaries required')
    if old['binary']['sha256']==new['binary']['sha256']:raise ValueError('old/new must be distinct actual binaries, not two labels on one executable')
    if old['binary']['version']!=new['binary']['version']:raise ValueError('this bounded same-package-preview profile requires matching explicit version')
    return old,new


class Database:
    def __init__(self,backend,url,path,cid=None):self.backend=backend;self.url=url;self.path=path;self.cid=cid;self.database=urlsplit(url).path.lstrip('/') if backend=='postgres' else None
    def docker(self,*parts):
        value=subprocess.run(['docker',*parts],capture_output=True,timeout=60)
        if value.returncode:raise RuntimeError('owned database operation failed: '+str(parts[0]))
        return value.stdout.decode().strip()
    def sql(self,statement):
        if self.backend=='sqlite':
            with sqlite3.connect(self.path,timeout=10) as connection:return connection.execute(statement).fetchall()
        return self.docker('exec',self.cid,'psql','-X','-U','taskloom_accept','-d',self.database,'-v','ON_ERROR_STOP=1','-Atc',statement)
    def snapshot(self):
        if self.backend=='sqlite':
            with sqlite3.connect(self.path,timeout=10) as connection:
                connection.execute('BEGIN')
                return {table:[list(row) for row in connection.execute('SELECT '+','.join(columns)+' FROM '+table+' ORDER BY '+','.join(columns))] for table,columns in TABLES.items()}
        fields=[]
        for table,columns in TABLES.items():
            columns=','.join(columns);fields.extend(["'"+table+"'","(SELECT coalesce(json_agg(json_build_array("+columns+") ORDER BY "+columns+"),'[]'::json) FROM "+table+")"])
        return json.loads(self.sql('SELECT json_build_object('+','.join(fields)+')::text'))
    def profile(self):
        state=self.snapshot();schema=state['taskloom_meta'];namespace=json.dumps(['_taskloom_control','storage'],separators=(',',':'))
        dataset=next(json.loads(body)['value'] for ns,f,i,rev,body in state['taskloom_records'] if (ns,f,i)==(namespace,'metadata','dataset'))
        return {'schema':schema,'dataset_identity':dataset}
    def live(self):
        clock="CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)" if self.backend=='sqlite' else 'CAST(floor(EXTRACT(EPOCH FROM clock_timestamp())*1000) AS BIGINT)'
        result=self.sql('SELECT count(*) FROM taskloom_leases WHERE expires_ms>'+clock)
        return result[0][0] if self.backend=='sqlite' else int(result)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path);parser.add_argument('--old-report',type=Path,required=True);parser.add_argument('--old-report-sha',required=True)
    parser.add_argument('--new-report',type=Path,required=True);parser.add_argument('--new-report-sha',required=True)
    parser.add_argument('--postgres-fixture',type=Path,required=True);parser.add_argument('--fault-test',type=Path,required=True);parser.add_argument('--fault-test-sha',required=True)
    args=parser.parse_args();root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    report={'version':1,'completed':False,'checks':{},'failures':[],'cases':[],'scope':__doc__,'generation_model':'explicit owned loopback OpenAI protocol fixture; zero cloud requests',
            'limits':['Only exact common schema/config/runtime contracts; incompatible source/capabilities are refused, never hash-patched.',
                      'Old uses verified SIGINT, new SIGTERM. Workers finish known responses before signaling; this is not in-flight cancellation acceptance.',
                      'Same-version string is not evidence of same binary: each actual SHA/source witness is independently checked.',
                      'Separate current SDK fault fixture creates unknown budget/effects; only actual old/new CLIs cold-read those records.',
                      'No schema upgrade, production admission changes, Kubernetes rolling acceptance, multihost partition or SLO claim.']}
    children=[];streams=[];servers=[];gates={};provider_calls=[];provider_errors=[];mutex=threading.Lock();private=[];clone_names=[]
    def clean(value):
        text=value.decode('utf-8',errors='replace') if isinstance(value,bytes) else str(value)
        for secret in private:
            if secret:text=text.replace(secret,'[private fixture credential]')
        return text
    def child(parts,environment,log_path,kind):
        log=log_path.open('xb');streams.append(log)
        process=subprocess.Popen(list(map(str,parts)),env=environment,stdout=log,stderr=log,start_new_session=True)
        children.append((process,kind,parts));return process
    def stop(process,kind,timeout=15):
        if process.poll() is not None:
            if process.returncode!=0:raise RuntimeError(kind+' exited unsuccessfully before graceful stop')
            return
        sent=time.time_ns();process.send_signal(signal.SIGINT if kind=='old' else signal.SIGTERM)
        try:process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill();process.wait(timeout=5);raise RuntimeError('graceful stop deadline: forced cleanup is not a passing drain')
        if process.returncode!=0:raise RuntimeError('graceful '+kind+' exit was not zero')
        return {'pid':process.pid,'signal':'SIGINT' if kind=='old' else 'SIGTERM','sent_unix_ns':sent,'exit_code':process.returncode,'exit_unix_ns':time.time_ns()}
    try:
        if os.name!='posix':raise ValueError('strict rehearsal requires POSIX signals; Windows TerminateProcess is not graceful drain')
        old_descriptor,new_descriptor=verify_sources(args.old_report.resolve(),args.old_report_sha,args.new_report.resolve(),args.new_report_sha)
        if sha(args.fault_test.resolve())!=args.fault_test_sha:raise ValueError('actual current fault helper differs from approved SHA')
        report['checks']['source_binary_schema_and_compiler_witnesses_match']=True
        report['old_build']=old_descriptor;report['new_build']=new_descriptor;report['fault_helper_sha256']=args.fault_test_sha
        (root/'bin').mkdir();executables={}
        for name,path in [('old',Path(old_descriptor['binary']['path'])),('new',Path(new_descriptor['binary']['path'])),('fault',args.fault_test.resolve())]:
            target=root/'bin'/name
            with path.open('rb') as incoming,target.open('xb') as outgoing:shutil.copyfileobj(incoming,outgoing)
            target.chmod(path.stat().st_mode);executables[name]=target
            expected=args.fault_test_sha if name=='fault' else (old_descriptor if name=='old' else new_descriptor)['binary']['sha256']
            if sha(target)!=expected:raise ValueError('copied '+name+' executable changed during copy')
        for name,descriptor in [('old',old_descriptor),('new',new_descriptor)]:
            actual=subprocess.check_output([str(executables[name]),'--version'],text=True,timeout=15).strip()
            if actual!=descriptor['binary']['version']:raise ValueError('actual '+name+' version differs from source/build witness')
        fixture=args.postgres_fixture.resolve();ownership=json.loads((fixture/'ownership.json').read_text());public=json.loads((fixture/'public.json').read_text());cid=ownership['container_id']
        if not re.fullmatch('[a-f0-9]{64}',cid):raise ValueError('invalid owned PostgreSQL container identity')
        inspector=Database('postgres','postgres://localhost/taskloom_accept',None,cid)
        def owned():
            state=json.loads(inspector.docker('inspect',cid,'--format','{{json .}}'))
            if state['Config']['Labels'].get('com.taskloom.database-acceptance')!=ownership['nonce'] or state['Image']!=ownership['image_id']:raise ValueError('owned PostgreSQL label/image changed')
        owned();pg=json.loads((fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL'];parsed=urlsplit(pg)
        if parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:raise ValueError('exact owned loopback PostgreSQL required')
        private.extend([pg,parsed.password]);report['postgres_provenance']=public
        class Provider(http.server.BaseHTTPRequestHandler):
            def log_message(self,*unused):pass
            def do_POST(self):
                try:
                    count=int(self.headers['Content-Length']);assert 0<count<=1024*1024;body=json.loads(self.rfile.read(count))
                    assert self.path=='/v1/chat/completions' and body['model']=='rolling-fixture'
                    content='\n'.join(str(message.get('content','')) for message in body['messages'])
                    markers=re.findall(r'rolling-job:([a-z0-9-]+)',content);marker=markers[-1] if markers else 'unmarked'
                    with mutex:
                        entry={'ordinal':len(provider_calls)+1,'marker':marker,'received_unix_ns':time.time_ns(),'response_sent':False};provider_calls.append(entry);gate=gates.get(marker)
                    if gate is not None and not gate.wait(timeout=90):raise RuntimeError('owned response gate timed out; no successful fixture response')
                    response=json.dumps({'id':'owned-rolling-fixture','object':'chat.completion','model':'rolling-fixture','choices':[{'index':0,'message':{'role':'assistant','content':'Fixture completed safely.'},'finish_reason':'stop'}],'usage':{'prompt_tokens':2,'completion_tokens':2,'total_tokens':4}}).encode()
                    self.send_response(200);self.send_header('content-type','application/json');self.send_header('content-length',str(len(response)));self.end_headers();self.wfile.write(response)
                    with mutex:entry.update(response_sent=True,response_unix_ns=time.time_ns())
                except Exception as error:
                    provider_errors.append(clean(error))
                    try:self.send_error(500)
                    except OSError:pass
        provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);thread=threading.Thread(target=provider.serve_forever,daemon=True);thread.start();servers.append((provider,thread))
        for backend,url,path in [('sqlite','sqlite://'+quote(str(root/'rolling.sqlite'),safe='')+'?mode=rwc&cache=private',root/'rolling.sqlite'),('postgres',pg,None)]:
            case=exercise_backend(root,backend,url,path,cid,executables,child,stop,provider,gates,provider_calls,mutex,clean,clone_names,owned)
            report['cases'].append(case)
        report['checks']['all_backend_phases_passed']=len(report['cases'])==2 and all(case['status']=='passed' for case in report['cases'])
        report['checks']['all_fixture_calls_received_known_success_without_hidden_retry']=not provider_errors and all(call['response_sent'] for call in provider_calls) and len(provider_calls)==sum(case.get('fixture_generation',{}).get('expected_requests',-1000) for case in report['cases'])
        report['completed']=True
    except Exception as error:report['failures'].append(clean(error))
    finally:
        for gate in gates.values():gate.set()
        for process,kind,parts in reversed(children):
            if process.poll() is None:
                try:stop(process,kind)
                except Exception as error:report['failures'].append('owned cleanup: '+clean(error))
        for server,thread in servers:server.shutdown();server.server_close();thread.join(timeout=5)
        for stream in streams:stream.close()
        for name in clone_names:
            try:owned();inspector.docker('exec',cid,'dropdb','-U','taskloom_accept',name)
            except Exception as error:report['failures'].append('owned clone cleanup failed: '+clean(error))
        report['failures'].extend(provider_errors)
        report['failures'].extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
        report['status']='passed' if report['completed'] and not report['failures'] else 'failed'
        save(root/'provider-calls.json',provider_calls);report['provider_calls_sha256']=sha(root/'provider-calls.json')
        report['logs']={path.relative_to(root).as_posix():sha(path) for path in root.rglob('*.log')}
        report['ops_source_sha256']={name:sha(Path(__file__).parent/name) for name in ['rolling_rehearsal.py','binary_witness.py','archive_binary.py']}
        save(root/'report.json',report)
    print(json.dumps(report));raise SystemExit(0 if report['status']=='passed' else 1)


def exercise_backend(root,backend,url,path,cid,executables,child,stop,provider,gates,provider_calls,mutex,clean,clone_names,owned):
    out=root/backend;out.mkdir();project=uuid.uuid4().hex;tenant='same-schema';namespace=json.dumps([tenant,project],separators=(',',':'))
    db=Database(backend,url,path,cid);local_children=[];ordinal=0;token='rolling-owned-'+uuid.uuid4().hex
    environment=dict(os.environ,TASKLOOM_DATABASE_URL=url,TASKLOOM_ROLLING_MODEL_KEY='owned-loopback-fixture',TASKLOOM_API_TOKEN=token)
    case={'backend':backend,'tenant':tenant,'project':project,'checks':{},'failures':[],'phases':[]}
    config_path=out/'app.json';api_bases={};worker_map={};expected_jobs=[];expected_markers=set()
    def cli(name,parts,config=None,env=None,allow_failure=False):
        nonlocal ordinal
        result=subprocess.run([str(executables[name]),'--config',str(config or config_path),*map(str,parts)],env=env or environment,capture_output=True,timeout=45)
        log=out/f'cli-{ordinal:03}-{name}.log';ordinal+=1
        with log.open('x',encoding='utf-8') as stream:stream.write(clean(result.stdout)+'\n'+clean(result.stderr))
        if result.returncode and not allow_failure:raise RuntimeError(name+' '+str(parts[0])+' failed; see '+log.name)
        if allow_failure:return result
        return json.loads(result.stdout)
    opener=build_opener(ProxyHandler({}))
    def api(name,path,body=None):
        request=Request(api_bases[name]+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+token,'content-type':'application/json'})
        with opener.open(request,timeout=10) as response:return json.loads(response.read(16*1024*1024))
    def launch(name,parts,label):
        process=child([executables[name],'--config',config_path,*parts],environment,out/(label+'.log'),name);local_children.append((process,name));return process
    def service(name):
        with socket.socket() as listener:listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
        api_bases[name]=f'http://127.0.0.1:{port}/api';process=launch(name,['serve',f'127.0.0.1:{port}'],name+'-api')
        for _ in range(150):
            if process.poll() is not None:raise RuntimeError(name+' API exited before ready')
            try:api(name,'/setup');return process
            except (URLError,OSError):time.sleep(.05)
        raise RuntimeError(name+' API readiness deadline')
    def calls():
        with mutex:return len(provider_calls)
    def phase(label):
        value=db.snapshot();save(out/(label+'-sql.json'),value)
        case['phases'].append({'name':label,'snapshot_sha256':canonical(value),'provider_requests':calls(),'table_rows':{name:len(rows) for name,rows in value.items()}})
        return value
    def wait_job(identity,state='completed',worker=None):
        for _ in range(300):
            value=api('new','/queue/jobs/'+identity)['job']
            if value['state']==state and (worker is None or value['worker']==worker):return value
            if value['state'] in {'failed','stopped','waiting','paused','abandoned'} and value['state']!=state:raise RuntimeError('unexpected durable boundary '+value['state']+' for '+identity+'; no automatic resume/replay')
            if any(process.poll() is not None for process,name in local_children if getattr(process,'rolling_worker',False) and not getattr(process,'rolling_drained',False)):raise RuntimeError('owned rolling worker exited unexpectedly')
            time.sleep(.05)
        raise RuntimeError('job '+identity+' did not reach '+state)
    def submit(task,name='new'):
        identity='task-'+uuid.uuid4().hex;path=out/(identity+'.json');save(path,task);value=cli(name,['queue-submit',path,'rolling-host-runner']);expected_jobs.append(value['id']);expected_markers.update(re.findall(r'rolling-job:([a-z0-9-]+)',json.dumps(task)));return value
    def run_task(identity,held=False,name='new'):
        marker=project+'-'+identity
        if held:
            with mutex:gates[marker]=threading.Event()
        return submit({'type':'run','id':identity,'prompt':'rolling-job:'+marker},name),marker
    def release(marker):gates[marker].set()
    def worker(name,label):
        process=launch(name,['worker',label],label);process.rolling_worker=True;process.rolling_drained=False
        # Read the live loaded ELF, not merely a user-supplied worker label.
        if sha(Path('/proc')/str(process.pid)/'exe')!=sha(executables[name]):raise RuntimeError('live worker ELF differs from approved binary')
        worker_map[label]={'kind':name,'pid':process.pid,'sha256':sha(executables[name])};return process
    def drain(process,name,label):
        # Every job previously claimed by this worker must already be complete.
        jobs=api('new','/queue/jobs')['jobs'];claimed=[job for job in jobs if job['worker']==label]
        if not claimed or any(job['state']!='completed' for job in claimed):raise RuntimeError('do not signal a worker with unresolved work; explicit reconciliation required')
        value=stop(process,name);process.rolling_drained=True
        after=api('new','/queue/jobs')['jobs']
        before_claims=sum(event['action']=='claim' and event['actor']==label for job in jobs for event in job['events'])
        after_claims=sum(event['action']=='claim' and event['actor']==label for job in after for event in job['events'])
        if before_claims!=after_claims:raise RuntimeError('draining worker claimed additional work')
        case['phases'].append({'name':'drain-'+label,**value,'claimed_jobs':[job['id'] for job in claimed],'additional_claims':0})
    def workflow(identity,name='new'):
        request={'id':identity,'input':{'goal':'rolling-job:'+project+'-'+identity},'definition':{'nodes':[{'id':'writer','task':'writer','depends_on':[],'params':None,'condition':{'type':'always'},'allow_skipped_dependencies':False,'select':{}}]},'peer':None,'limits':setup_old['workflow_defaults']}
        preview=api(name,'/workflow-design/preview',request)
        return submit({'type':'workflow','launch':{'request':request,'preview_key':preview['preview_key']}},name)
    def unknown_rows(snapshot):
        def matches(row):
            try:return isinstance(row[0],str) and json.loads(row[0])[0]=='operational-faults'
            except (ValueError,TypeError):return False
        return {name:[row for row in rows if matches(row)] for name,rows in snapshot.items() if name!='taskloom_meta'}
    def terminal_runs(snapshot):
        return {(row[0],row[1],row[2]):row for row in snapshot['taskloom_records'] if row[0]==namespace and row[1] in {'run','budget'}}
    def inspect_unknown(faults):
        result={}
        for field,identity in [('model_death','model'),('pending_effect','effect')]:
            record=faults[field];cfg=dict(config,database=dict(config['database'],tenant=record['tenant'],project=record['project']))
            path=out/('cold-'+field+'.json')
            if not path.exists():save(path,cfg)
            values={name:cli(name,['inspect',identity],config=path) for name in ['old','new']}
            if values['old']!=values['new']:raise RuntimeError('old/new unknown checkpoint reads differ')
            result[field]=values['old']
        if result['model_death']['report']['budget']['uncertain_attempts']!=1 or result['pending_effect']['status']!='needs_recovery' or result['pending_effect']['tool_steps'][0]['progress']['type']!='uncertain':raise RuntimeError('unknown budget/effect was erased or misclassified')
        return result
    try:
        # Fresh fixture initialization only. No migration command is invoked
        # after the old/new dataset/contract admission gate.
        subprocess.run([str(executables['old']),'init',str(out/'old-defaults.json')],check=True,capture_output=True,timeout=30)
        defaults=json.loads((out/'old-defaults.json').read_text())
        config=dict(defaults,state_dir=str(out/'state'),knowledge=False,provider={'type':'openai','base_url':f'http://127.0.0.1:{provider.server_port}/v1','key_env':'TASKLOOM_ROLLING_MODEL_KEY','namespace':'same-schema-owned-v1'},model='rolling-fixture',system='Return one short plain fixture answer. Tools do not grant authority.',output_tokens=64,prices=[{'prefix':'rolling-fixture','input_per_million':0.0,'output_per_million':0.0}],limits=dict(defaults['limits'],max_cost_micros=0,max_duration={'secs':300,'nanos':0}),database={'url_env':'TASKLOOM_DATABASE_URL','tenant':tenant,'project':project},roles=[{'name':'writer','system':'Follow input.input.goal briefly. No external writes.','tools':[],'output':{'type':'text'}}])
        save(config_path,config);migration=cli('old',['database-migrate']);save(out/'fresh-schema-initialization.json',migration)
        faults_dir=out/'faults';faults_dir.mkdir();fault_env=dict(environment,TASKLOOM_FAULT_OUTPUT=str(faults_dir),TASKLOOM_FAULT_SQLITE_URL=url,TASKLOOM_TEST_POSTGRES_URL=url)
        parts=[str(executables['fault']),'--exact',backend+'_operational_process_and_owner_faults','--nocapture']
        if backend=='postgres':parts.append('--ignored')
        result=subprocess.run(parts,env=fault_env,capture_output=True,timeout=180)
        with (out/'current-sdk-fault-fixture.log').open('x',encoding='utf-8') as stream:stream.write(clean(result.stdout)+'\n'+clean(result.stderr))
        if result.returncode:raise RuntimeError('actual current SDK fault fixture failed')
        faults=json.loads((faults_dir/backend/'report.json').read_text());case['checks']['separate_current_sdk_fault_fixture_passed']=faults['status']=='passed' and all(faults['checks'].values())
        initial_profile=db.profile()
        if initial_profile['schema']!=[[1,3]] or initial_profile['dataset_identity']!=migration['dataset_identity']:raise RuntimeError('fresh schema/dataset changed before admission')
        old_api=service('old');new_api=service('new');setup_old=api('old','/setup');setup_new=api('new','/setup')
        old_queue=api('old','/queue');new_queue=api('new','/queue')
        if old_queue['host_binding']!=new_queue['host_binding']:raise RuntimeError('actual old/new frozen queue contracts differ; rollout refused without rewriting hashes')
        if setup_old['active_tools']!=setup_new['active_tools'] or setup_old['active_roles']!=setup_new['active_roles']:raise RuntimeError('actual active capability catalogs differ; rollout refused')
        if any(tool['effect']!='read_only' for tool in setup_old['active_tools']):raise RuntimeError('bounded common profile must have no external-write tools')
        gate={'profile':initial_profile,'old_queue':old_queue,'new_queue':new_queue,'active_tools':setup_old['active_tools'],'active_roles':setup_old['active_roles'],'compiled_feature_sets':'old app,database; new all-features; only exact common runtime profile is admitted'}
        save(out/'admission-gate.json',gate);case['checks']['actual_schema_dataset_frozen_binding_and_active_capabilities_match']=True
        api('old','/sessions',{'id':'rolling-session'})
        limits=out/'queue-limits.json';save(limits,{'max_open_jobs':32,'max_running':3});policy=cli('old',['queue-configure',limits,0,'rolling-host-operator','explicit same-schema owned fixture'])
        if policy['host_binding']!=old_queue['host_binding']:raise RuntimeError('configured queue does not retain actual admitted binding')
        unknown_before=unknown_rows(phase('admitted'));save(out/'unknown-cold-initial.json',inspect_unknown(faults))
        first,first_marker=run_task('held-one',held=True,name='old');second,second_marker=run_task('held-two',held=True,name='old')
        old_one=worker('old','old-one');old_two=worker('old','old-two');wait_job(first['id'],'running');wait_job(second['id'],'running')
        for _ in range(200):
            with mutex:seen={call['marker'] for call in provider_calls}
            if first_marker in seen and second_marker in seen:break
            time.sleep(.05)
        else:raise RuntimeError('old workers did not reach actual held HTTP calls')
        third,third_marker=run_task('held-three',held=True);new_one=worker('new','new-one');wait_job(third['id'],'running','new-one')
        for _ in range(200):
            with mutex:seen={call['marker'] for call in provider_calls}
            if third_marker in seen:break
            time.sleep(.05)
        else:raise RuntimeError('new worker did not join actual held HTTP call')
        overlap=phase('three-workers-overlap');running=api('new','/queue/jobs')['jobs'];running=[job for job in running if job['state']=='running']
        if len(running)!=3 or {job['worker'] for job in running}!={'old-one','old-two','new-one'}:raise RuntimeError('three distinct old/new workers were not concurrently live')
        case['checks']['two_actual_old_and_one_distinct_new_worker_overlap']=True
        # Release new first so it is actually serving while old replicas are
        # individually completed and drained, never canceling a paid request.
        release(third_marker);wait_job(third['id']);probe,_=run_task('overlap-probe');wait_job(probe['id'],worker='new-one')
        labels={api('new','/queue/jobs/'+first['id'])['job']['worker']:first_marker,api('new','/queue/jobs/'+second['id'])['job']['worker']:second_marker}
        release(labels['old-one']);wait_job(first['id'] if api('new','/queue/jobs/'+first['id'])['job']['worker']=='old-one' else second['id']);drain(old_one,'old','old-one')
        probe,_=run_task('after-first-drain');wait_job(probe['id'],worker='new-one')
        release(labels['old-two']);wait_job(second['id'] if api('new','/queue/jobs/'+second['id'])['job']['worker']=='old-two' else first['id']);drain(old_two,'old','old-two')
        case['checks']['old_known_work_finishes_then_sigint_exits_zero_without_new_claim']=True
        old_terminal=terminal_runs(phase('old-replicas-drained'))
        created,_=run_task('new-written');wait_job(created['id'],worker='new-one')
        turn=submit({'type':'session_turn','id':'rolling-session','turn':'new-turn','prompt':'rolling-job:'+project+'-new-turn'});wait_job(turn['id'],worker='new-one')
        flow=workflow('new-flow');wait_job(flow['id'],worker='new-one');new_records=phase('new-records-written')
        # Actual old runtime cold-parses the new run, workflow child and session.
        listing=cli('new',['list'])
        for entry in listing:
            identity=entry.get('id',entry.get('run_id'))
            if identity is None:raise RuntimeError('run listing did not contain an inspectable identity')
            if cli('old',['inspect',identity])!=cli('new',['inspect',identity]):raise RuntimeError('old cannot cold-read exact new run/budget record')
        for path in ['/sessions/rolling-session','/workflows/new-flow']:
            a=api('old',path);b=api('new',path);field='session' if 'sessions' in path else 'state'
            if a[field]!=b[field]:raise RuntimeError('old/new persistent session/workflow reads differ')
        case['checks']['old_reads_new_runs_children_sessions_workflows_and_budget_exactly']=True
        hold,hold_marker=run_task('rollback-held',held=True);wait_job(hold['id'],'running','new-one')
        rollback=worker('old','old-rollback');probe,_=run_task('rollback-probe');wait_job(probe['id'],worker='old-rollback')
        phase('rollback-old-new-overlap');release(hold_marker);wait_job(hold['id']);drain(new_one,'new','new-one')
        turn=submit({'type':'session_turn','id':'rolling-session','turn':'old-turn','prompt':'rolling-job:'+project+'-old-turn'},name='old');wait_job(turn['id'],worker='old-rollback')
        flow=workflow('old-flow',name='old');wait_job(flow['id'],worker='old-rollback');last,_=run_task('rollback-final',name='old');wait_job(last['id'],worker='old-rollback');drain(rollback,'old','old-rollback')
        case['checks']['old_rollback_serves_new_submissions_and_continues_session_after_new_sigterm_drain']=True
        before_negatives=phase('rollback-complete')
        if any(terminal_runs(before_negatives).get(key)!=value for key,value in old_terminal.items()):raise RuntimeError('completed old run/budget record changed or disappeared during replacement/rollback')
        jobs=api('new','/queue/jobs')['jobs']
        if {job['id'] for job in jobs}!=set(expected_jobs) or any(job['state']!='completed' or job['attempts']!=1 for job in jobs):raise RuntimeError('healthy logical jobs did not all complete exactly one claim')
        case['checks']['all_logical_jobs_once_no_lost_terminal_run_or_budget_records']=True
        with mutex:actual=Counter(call['marker'] for call in provider_calls if call['marker'].startswith(project+'-'))
        if actual!=Counter({marker:1 for marker in expected_markers}):raise RuntimeError('missing/duplicate fixture model dispatch or attribution; denominator retained')
        case['fixture_generation']={'expected_requests':len(expected_markers),'actual_requests':sum(actual.values()),'unique_markers':len(actual),'failures':0}
        case['checks']['every_healthy_job_has_one_attributed_generation_without_retry']=True
        # Actual incompatible config/capability tries a real pending probe;
        # neither binary may claim it or contact the model.
        negative,_=run_task('negative-unclaimed');before=db.snapshot();requests=calls()
        for suffix,change in [('model',{'model':'changed-incompatible-model'}),('capability',{'knowledge':True})]:
            path=out/('incompatible-'+suffix+'.json');save(path,dict(config,**change))
            for name in ['old','new']:
                if cli(name,['worker-once','rejected-'+suffix],config=path,allow_failure=True).returncode==0:raise RuntimeError('incompatible '+suffix+' worker was not rejected')
        if calls()!=requests or db.snapshot()!=before:raise RuntimeError('incompatible config/capability mutated state or dispatched a model')
        case['checks']['actual_config_and_capability_mismatch_refuse_before_claim_or_dispatch']=True
        saved=api('new','/queue/jobs/'+negative['id'])['job'];cli('old',['queue-control',saved['id'],saved['revision'],'abandon','rolling-host-operator','negative probe was never dispatched; retire scheduling only'])
        # Unknown state remains exactly equal through all rolling/rollback reads.
        unknown_after=unknown_rows(db.snapshot());save(out/'unknown-cold-final.json',inspect_unknown(faults))
        if unknown_after!=unknown_before:raise RuntimeError('unknown namespace records changed, refunded or replayed')
        effect=faults_dir/backend/'pending-effect/actual-effect.jsonl'
        if len(effect.read_text().splitlines())!=1:raise RuntimeError('unknown external effect was replayed')
        case['checks']['unknown_attempt_quota_effect_and_all_scoped_tables_preserved_without_resume']=True
        wait_quiescent(db)
        schema_copy=clone_database(db,out/'schema-copy.sqlite','schema',clone_names,owned)
        schema_copy.sql('UPDATE taskloom_meta SET version=4 WHERE singleton=1');copy_env=dict(environment,TASKLOOM_DATABASE_URL=schema_copy.url);before=schema_copy.snapshot();requests=calls()
        for name in ['old','new']:
            if cli(name,['worker-once','rejected-schema'],env=copy_env,allow_failure=True).returncode==0:raise RuntimeError('schema4 clone was not rejected')
        if calls()!=requests or schema_copy.snapshot()!=before or db.profile()!=initial_profile:raise RuntimeError('schema refusal mutated clone/main or dispatched')
        case['checks']['actual_incompatible_schema_clone_refused_without_migration']=True
        dataset_copy=clone_database(db,out/'dataset-copy.sqlite','dataset',clone_names,owned);copy_env=dict(environment,TASKLOOM_DATABASE_URL=dataset_copy.url)
        pending=out/'dataset-negative.json';save(pending,{'type':'run','id':'dataset-negative','prompt':'rolling-job:'+project+'-must-not-run'})
        cli('old',['queue-submit',pending,'rolling-host-runner'],env=copy_env)
        rows=dataset_copy.snapshot()['taskloom_records'];target=next(row for row in rows if row[1]=='metadata' and row[2]=='dataset');body=json.loads(target[4]);body['value']=str(uuid.uuid4());encoded=json.dumps(body).replace("'","''")
        dataset_copy.sql("UPDATE taskloom_records SET body='"+encoded+"' WHERE family='metadata' AND object_id='dataset'");before=dataset_copy.snapshot();requests=calls()
        for name in ['old','new']:
            if cli(name,['worker-once','rejected-dataset'],env=copy_env,allow_failure=True).returncode==0:raise RuntimeError('different dataset identity clone was not rejected')
        if calls()!=requests or dataset_copy.snapshot()!=before:raise RuntimeError('dataset mismatch claimed a job, mutated records or dispatched')
        case['checks']['actual_dataset_identity_mismatch_refused_before_claim']=True
        final=phase('final');case['workers']=worker_map;case['common_dataset']=initial_profile;case['queue_binding']=old_queue['host_binding'];case['healthy_jobs']=len(expected_jobs)-1
        case['checks']['schema_and_original_dataset_unchanged_after_all_phases']=db.profile()==initial_profile
        case['final_table_sha256']=canonical(final)
    except Exception as error:
        case['failures'].append(clean(error))
        try:save(out/'failure-sql.json',db.snapshot())
        except Exception:pass
    finally:
        for marker,gate in gates.items():
            if marker.startswith(project+'-'):gate.set()
        for process,name in reversed(local_children):
            if process.poll() is None:
                try:stop(process,name)
                except Exception as error:case['failures'].append('owned backend cleanup: '+clean(error))
        case['failures'].extend('check failed: '+name for name,passed in case['checks'].items() if not passed)
        case['status']='failed' if case['failures'] else 'passed';save(out/'report.json',case)
    return case


def wait_quiescent(db):
    for _ in range(200):
        if db.live()==0:return
        time.sleep(.1)
    raise RuntimeError('owned database still has live leases; clone/backup refused')


def clone_database(db,path,kind,clone_names,owned):
    if db.backend=='sqlite':
        path.open('xb').close()
        with sqlite3.connect(db.path) as original,sqlite3.connect(path) as target:original.backup(target)
        return Database('sqlite','sqlite://'+quote(str(path),safe='')+'?mode=rw&cache=private',path)
    owned();name='taskloom_roll_'+kind+'_'+uuid.uuid4().hex[:16];dump=path.with_suffix('.dump')
    with dump.open('xb') as stream:result=subprocess.run(['docker','exec',db.cid,'pg_dump','-U','taskloom_accept','-d',db.database,'-Fc'],stdout=stream,stderr=subprocess.PIPE,timeout=60)
    if result.returncode:raise RuntimeError('owned PostgreSQL quiescent clone dump failed')
    db.docker('exec',db.cid,'createdb','-U','taskloom_accept',name);clone_names.append(name)
    with dump.open('rb') as stream:result=subprocess.run(['docker','exec','--interactive',db.cid,'pg_restore','--exit-on-error','--single-transaction','--no-owner','-U','taskloom_accept','-d',name],stdin=stream,capture_output=True,timeout=60)
    if result.returncode:raise RuntimeError('owned PostgreSQL clone restore failed')
    parsed=urlsplit(db.url);url=urlunsplit((parsed.scheme,parsed.netloc,'/'+name,parsed.query,parsed.fragment))
    return Database('postgres',url,None,db.cid)


if __name__=='__main__':main()
