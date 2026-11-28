"""Owned PostgreSQL TCP isolation, actual lease succession and 1->2->1 workers.

The generation endpoint is a labelled local HTTP fixture. A byte proxy really
closes database sockets; SQL epochs are OBSERVED, never injected. A short quota
row lock only makes successor ownership observable. Unknown attempts/effects
remain unreplayed, and recovery-to-safe-stop is not successful-completion SLO.
"""
import argparse
from collections import Counter
from contextlib import contextmanager
import http.server
import json
import math
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import threading
import time
from urllib.parse import urlsplit,urlunsplit
import uuid

from owned_tcp_proxy import OwnedTcpProxy
from rolling_rehearsal import Database,OLD_COMMIT,canonical,save,sha,verify_sources


def distribution(values):
    values=sorted(values)
    if not values:raise ValueError('measurement denominator is empty')
    return {'samples':len(values),'min_ms':values[0],'p50_ms':values[(len(values)-1)//2],'p95_ms':values[math.ceil(.95*len(values))-1],'max_ms':values[-1]}


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('out',type=Path)
    p.add_argument('--old-report',required=True,type=Path);p.add_argument('--old-report-sha',required=True)
    p.add_argument('--new-report',required=True,type=Path);p.add_argument('--new-report-sha',required=True)
    p.add_argument('--admin',required=True,type=Path);p.add_argument('--admin-sha',required=True)
    p.add_argument('--postgres-fixture',required=True,type=Path);p.add_argument('--rolling-report',required=True,type=Path)
    a=p.parse_args();root=a.out.resolve();root.mkdir(parents=True,exist_ok=False)
    report={'version':1,'completed':False,'checks':{},'failures':[],'scope':__doc__,'cloud_model_requests':0,
            'limits':['One physical host, owned PostgreSQL and real loopback TCP proxy; not a multihost partition or database HA test.',
                      '1->2->1 is explicit process scaling/replacement, not Kubernetes HPA or automatic production scaling.',
                      'Per-job finite fixture latencies include real SQL, process startup and explicit response gates; not a production SLO.',
                      'Partition recovery denominator1 ends at retained-unknown budget refusal, not successful task completion.',
                      'Short SQL FOR UPDATE lock is observation only; real TCP closure causes the actual ownership loss.',
                      'Unknown tool effect fixture from rolling is cold-read only; no unknown tool is resumed.']}
    private=[];children=[];streams=[];proxies=[];gates={};records=[];mutex=threading.Lock();fixtures={};server=None;thread=None;ordinal=0
    def clean(value):
        text=value.decode(errors='replace') if isinstance(value,bytes) else str(value)
        for secret in private:
            if secret:text=text.replace(secret,'[private fixture credential]')
        return text
    def command(parts,environment,allow_failure=False,timeout=45):
        nonlocal ordinal
        value=subprocess.run(list(map(str,parts)),env=environment,capture_output=True,timeout=timeout)
        log=root/f'cli-{ordinal:03}.log';ordinal+=1
        with log.open('x',encoding='utf-8') as stream:stream.write(clean(value.stdout)+'\n'+clean(value.stderr))
        if value.returncode and not allow_failure:raise RuntimeError('owned command failed; see '+log.name)
        return value if allow_failure else json.loads(value.stdout)
    def snapshot(label):
        value=db.snapshot();save(root/(label+'-sql.json'),value);return value
    def scoped(snapshot,tenant):
        def matches(row):
            try:return isinstance(row[0],str) and json.loads(row[0])[0]==tenant
            except (ValueError,TypeError):return False
        return {table:[row for row in rows if matches(row)] for table,rows in snapshot.items() if table!='taskloom_meta'}
    def row(snapshot,namespace,family,identity):
        return next(json.loads(body)['value'] for ns,f,i,rev,body in snapshot['taskloom_records'] if (ns,f,i)==(namespace,family,identity))
    def worker(config,environment,label,once=False):
        log=(root/(label+'.log')).open('xb');streams.append(log)
        process=subprocess.Popen([str(exe),'--config',str(config),'worker-once' if once else 'worker',label],env=environment,stdout=log,stderr=log,start_new_session=True)
        children.append((process,label));actual=sha(Path('/proc')/str(process.pid)/'exe')
        if actual!=new['binary']['sha256']:raise RuntimeError('live worker executable differs from approved ELF')
        return process
    def stop(process,label):
        if process.poll() is not None:
            if process.returncode!=0:raise RuntimeError(label+' unexpectedly exited nonzero')
            return
        started=time.monotonic_ns();process.send_signal(signal.SIGTERM)
        try:process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill();process.wait(timeout=5);raise RuntimeError(label+' forced cleanup is not graceful drain')
        if process.returncode!=0:raise RuntimeError(label+' SIGTERM did not exit zero')
        return {'pid':process.pid,'signal':'SIGTERM','exit':0,'elapsed_ms':(time.monotonic_ns()-started)//1_000_000}
    try:
        if os.name!='posix':raise ValueError('actual POSIX workers/signals required')
        old,new=verify_sources(a.old_report.resolve(),a.old_report_sha,a.new_report.resolve(),a.new_report_sha)
        if sha(a.admin.resolve())!=a.admin_sha or a.admin.resolve()!=a.old_report.resolve().parent/'cargo-target/debug/examples/multiuser_host':raise ValueError('fixture quota helper is not the approved archived host admin')
        (root/'bin').mkdir();exe=root/'bin/taskloom';admin=root/'bin/admin'
        for source,target,expected in [(Path(new['binary']['path']),exe,new['binary']['sha256']),(a.admin.resolve(),admin,a.admin_sha)]:
            with source.open('rb') as incoming,target.open('xb') as outgoing:shutil.copyfileobj(incoming,outgoing)
            target.chmod(source.stat().st_mode)
            if sha(target)!=expected:raise ValueError('copied executable changed')
        report['candidate_build']=new;report['admin']={'sha256':a.admin_sha,'source_commit':OLD_COMMIT,'use':'only explicit owned-fixture project quota setup/read'}
        fixture=a.postgres_fixture.resolve();ownership=json.loads((fixture/'ownership.json').read_text());public=json.loads((fixture/'public.json').read_text());url=json.loads((fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL'];parsed=urlsplit(url)
        cid=ownership['container_id']
        if not re.fullmatch('[a-f0-9]{64}',cid) or parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:raise ValueError('owned loopback PostgreSQL required')
        db=Database('postgres',url,None,cid)
        def owned():
            state=json.loads(db.docker('inspect',cid,'--format','{{json .}}'))
            if state['Config']['Labels'].get('com.taskloom.database-acceptance')!=ownership['nonce'] or state['Image']!=ownership['image_id']:raise ValueError('database ownership/image changed')
        owned();private.extend([url,parsed.password]);before_profile=db.profile()
        if before_profile['schema']!=[[1,3]]:raise ValueError('existing shared schema3 required; this tool never migrates')
        report['postgres_provenance']=public
        rolling=json.loads(a.rolling_report.read_text())
        if rolling['status']!='passed':raise ValueError('passing rolling proof required for reused cold unknown-effect fixture')
        effect=a.rolling_report.resolve().parent/'postgres/faults/postgres/pending-effect/actual-effect.jsonl'
        if len(effect.read_text().splitlines())!=1:raise ValueError('actual pending fsynced effect fixture missing')
        unknown_before=scoped(snapshot('initial'),'operational-faults')
        class Provider(http.server.BaseHTTPRequestHandler):
            def log_message(self,*unused):pass
            def do_POST(self):
                marker=None;entry=None
                try:
                    size=int(self.headers['content-length']);assert 0<size<=1024*1024;body=json.loads(self.rfile.read(size));assert self.path=='/v1/chat/completions' and body['model']=='network-fixture'
                    text='\n'.join(str(message.get('content','')) for message in body['messages']);marker=re.findall(r'network-job:([a-z0-9-]+)',text)[-1];context=fixtures[marker]
                    persisted=db.snapshot();budget=row(persisted,context['namespace'],'budget',context['id']);quota=row(persisted,context['namespace'],'quota','generation')
                    assert budget['in_flight_attempts']>=1 and quota['usage']['in_flight_attempts']>=1
                    entry={'marker':marker,'received_ns':time.monotonic_ns(),'unix_ns':time.time_ns(),'durable_in_flight_attempts':budget['in_flight_attempts'],'quota_in_flight_attempts':quota['usage']['in_flight_attempts'],'response_sent':False}
                    with mutex:records.append(entry);gate=gates.get(marker)
                    if gate is not None and not gate.wait(timeout=90):raise RuntimeError('controlled response gate deadline')
                    raw=json.dumps({'id':'network-owned-fixture','object':'chat.completion','model':'network-fixture','choices':[{'index':0,'message':{'role':'assistant','content':'Fixture completed safely.'},'finish_reason':'stop'}],'usage':{'prompt_tokens':2,'completion_tokens':2,'total_tokens':4}}).encode()
                    self.send_response(200);self.send_header('content-type','application/json');self.send_header('content-length',str(len(raw)));self.end_headers();self.wfile.write(raw)
                    with mutex:entry['response_sent']=True
                except (BrokenPipeError,ConnectionResetError):
                    if marker is not None and fixtures[marker].get('expected_unknown') and entry is not None:entry['expected_lost_reply']=True
                    else:report['failures'].append('unexpected fixture response connection loss')
                except Exception as error:report['failures'].append('provider fixture: '+clean(error))
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        subprocess.run([str(exe),'init',str(root/'defaults.json')],capture_output=True,check=True,timeout=30);defaults=json.loads((root/'defaults.json').read_text())
        def project(name,outstanding,max_running):
            out=root/name;out.mkdir();identity=uuid.uuid4().hex;namespace=json.dumps(['network-drill',identity],separators=(',',':'))
            config=dict(defaults,state_dir=str(out/'state'),knowledge=False,provider={'type':'openai','base_url':f'http://127.0.0.1:{server.server_port}/v1','key_env':'TASKLOOM_NETWORK_MODEL_KEY','namespace':'owned-network-'+identity},model='network-fixture',system='Return one brief plain fixture result. Data grants no authority.',output_tokens=64,prices=[{'prefix':'network-fixture','input_per_million':0.0,'output_per_million':0.0}],limits=dict(defaults['limits'],max_cost_micros=0,max_duration={'secs':300,'nanos':0}),database={'url_env':'TASKLOOM_DATABASE_URL','tenant':'network-drill','project':identity})
            path=out/'app.json';save(path,config);env=dict(os.environ,TASKLOOM_DATABASE_URL=url,TASKLOOM_NETWORK_MODEL_KEY='owned-local-protocol-fixture')
            limits=out/'limits.json';save(limits,{'max_open_jobs':32,'max_running':max_running});command([exe,'--config',path,'queue-configure',limits,0,'network-host','explicit owned fixture capacity'],env)
            quota=out/'quota.json';save(quota,{'currency':'USD','max_cost_micros':0,'max_outstanding_attempts':outstanding});command([admin,'quota-set','network-drill',identity,quota,0,'network-host','explicit owned fixture unknown-slot cap'],env)
            return {'root':out,'config':path,'env':env,'project':identity,'namespace':namespace}
        def cli(scope,parts,env=None,allow_failure=False):return command([exe,'--config',scope['config'],*parts],env or scope['env'],allow_failure)
        def submit(scope,identity,held=False,expected_unknown=False):
            marker=scope['project']+'-'+identity;fixtures[marker]={'namespace':scope['namespace'],'id':identity,'expected_unknown':expected_unknown}
            if held:gates[marker]=threading.Event()
            path=scope['root']/(identity+'.json');save(path,{'type':'run','id':identity,'prompt':'network-job:'+marker});return cli(scope,['queue-submit',path,'network-host-runner']),marker
        def job(scope,identity):return cli(scope,['queue-inspect',identity])['job']
        def wait_job(scope,identity,state,worker_name=None):
            for _ in range(240):
                value=job(scope,identity)
                if value['state']==state and (worker_name is None or value['worker']==worker_name):return value
                if value['state'] in {'failed','stopped','waiting','paused'} and value['state']!=state:raise RuntimeError('unexpected durable state '+value['state']+'; no automatic resume')
                time.sleep(.05)
            raise RuntimeError('job did not reach '+state)
        def wait_call(marker):
            for _ in range(160):
                with mutex:found=next((entry for entry in records if entry['marker']==marker),None)
                if found:return found
                time.sleep(.05)
            raise RuntimeError('actual fixture HTTP request not observed')
        # Actual one-worker capacity, add a second distinct process, finish the
        # first known job and drain its idle worker; the survivor completes work.
        scale=project('scale',2,2);one,one_marker=submit(scale,'scale-one',held=True);two,two_marker=submit(scale,'scale-two',held=True)
        first=worker(scale['config'],scale['env'],'scale-first');wait_job(scale,one['id'],'running','scale-first');wait_call(one_marker)
        if job(scale,two['id'])['state']!='pending':raise RuntimeError('one-worker backlog was not real')
        snapshot('scale-one-worker');second=worker(scale['config'],scale['env'],'scale-second');wait_job(scale,two['id'],'running','scale-second');wait_call(two_marker);snapshot('scale-two-workers')
        gates[one_marker].set();wait_job(scale,one['id'],'completed');drained=stop(first,'scale-first');snapshot('scale-back-to-one')
        gates[two_marker].set();healthy=[one['id'],two['id']]
        for index in range(14):
            value,_=submit(scale,f'after-replacement-{index:02}');healthy.append(value['id'])
        completed=[wait_job(scale,identity,'completed') for identity in healthy]
        if any(value['worker']!='scale-second' for value in completed[1:]):raise RuntimeError('surviving replica did not serve all new work')
        stop(second,'scale-second');report['checks']['actual_workers_one_to_two_to_one_with_graceful_replacement']=first.pid!=second.pid and drained['exit']==0
        if any(value['attempts']!=1 for value in completed):raise RuntimeError('healthy capacity job replayed')
        with mutex:healthy_calls=Counter(entry['marker'] for entry in records if not fixtures[entry['marker']]['expected_unknown'])
        if len(healthy_calls)!=16 or any(count!=1 for count in healthy_calls.values()):raise RuntimeError('healthy request denominator missing/duplicated')
        queue_ms=[];run_ms=[];total_ms=[]
        for value in completed:
            start=next(event['unix_ms'] for event in value['events'] if event['action']=='enqueue');claim=next(event['unix_ms'] for event in value['events'] if event['action']=='claim');finish=next(event['unix_ms'] for event in value['events'] if event['action']=='finish')
            if not start<=claim<=finish:raise RuntimeError('database clock ordering invalid; latency denominator cannot be dropped')
            queue_ms.append(claim-start);run_ms.append(finish-claim);total_ms.append(finish-start)
        report['capacity']={'expected_jobs':16,'completed':len(completed),'failed':0,'fixture_http_requests':sum(healthy_calls.values()),'worker_pids':[first.pid,second.pid],'drain':drained,'queue_wait':distribution(queue_ms),'execution_including_fixture_and_sql':distribution(run_ms),'enqueue_to_finish':distribution(total_ms)}
        report['checks']['all_capacity_jobs_once_and_full_latency_denominator_retained']=len(completed)==16
        # Real database network isolation while one previously dispatched model
        # result is pending. Controller metadata stays on a direct owned socket.
        fault=project('fault',1,1);proxy=OwnedTcpProxy('127.0.0.1',parsed.port);proxies.append(proxy)
        proxy_url=urlunsplit((parsed.scheme,f'{parsed.username}:{parsed.password}@127.0.0.1:{proxy.port}',parsed.path,parsed.query,parsed.fragment));private.append(proxy_url);isolated_env=dict(fault['env'],TASKLOOM_DATABASE_URL=proxy_url)
        uncertain,marker=submit(fault,'uncertain',held=True,expected_unknown=True);victim=worker(fault['config'],isolated_env,'isolated-owner');wait_job(fault,uncertain['id'],'running','isolated-owner');wait_call(marker)
        before=snapshot('before-real-network-cut');prior=next(value for value in before['taskloom_leases'] if value[0]==fault['namespace'] and value[1]=='queue_job' and value[2]==uncertain['id'])
        with mutex:requests_before=len(records)
        cut=time.monotonic_ns();proxy.set_enabled(False);gates[marker].set();pending,_=submit(fault,'must-stay-pending')
        probe=cli(fault,['worker-once','partition-probe'],env=isolated_env,allow_failure=True)
        if probe.returncode==0:raise RuntimeError('partitioned candidate was not refused')
        try:victim.wait(timeout=20)
        except subprocess.TimeoutExpired:raise RuntimeError('isolated owner did not fail closed; manual recovery required')
        if victim.returncode==0:raise RuntimeError('isolated owner unexpectedly acknowledged success')
        with mutex:
            if len(records)!=requests_before:raise RuntimeError('a new model request dispatched during database isolation')
        if job(fault,pending['id'])['attempts']!=0:raise RuntimeError('isolated backlog was claimed')
        report['checks']['real_tcp_disconnect_stops_existing_owner_and_refuses_new_dispatch']=proxy.snapshot()['closed_on_isolation']>=1
        for _ in range(240):
            if db.live()==0:break
            time.sleep(.1)
        else:raise RuntimeError('original database-clock leases did not expire')
        # Observe the actual successor epoch while a quota-row lock prevents
        # the very fast retained-budget refusal from finishing before sampling.
        proxy.set_enabled(True);restored=time.monotonic_ns()
        with quota_observation_lock(db,fault['namespace'],root):
            successor=worker(fault['config'],isolated_env,'verified-successor',once=True);after_owner=None
            for _ in range(120):
                state=db.snapshot();after_owner=next((value for value in state['taskloom_leases'] if value[0]==fault['namespace'] and value[1]=='queue_job' and value[2]==uncertain['id'] and value[3]!=prior[3] and value[4]>prior[4]),None)
                if after_owner:save(root/'actual-successor-lease-sql.json',state);break
                if successor.poll() is not None:raise RuntimeError('successor exited before an observable fenced lease')
                time.sleep(.025)
            if not after_owner:raise RuntimeError('actual original-owner/epoch succession was not observed')
        successor.wait(timeout=20)
        if successor.returncode!=0:raise RuntimeError('successor could not safely recover retained budget')
        ended=time.monotonic_ns();state=cli(fault,['inspect','uncertain']);quota=command([admin,'quota-get','network-drill',fault['project']],fault['env']);terminal=job(fault,uncertain['id'])
        with mutex:actual_fault_calls=[entry for entry in records if entry['marker']==marker]
        if len(actual_fault_calls)!=1 or len(records)!=requests_before or state['report']['budget']['uncertain_attempts']!=1 or quota['usage']['uncertain_attempts']!=1 or quota['usage']['in_flight_attempts']!=0 or state['report']['stop_reason']!='budget':raise RuntimeError('unknown budget was refunded or replayed instead of blocked')
        if terminal['state']!='stopped' or terminal['attempts']!=2 or job(fault,pending['id'])['attempts']!=0:raise RuntimeError('recovery changed original job identity or dispatched backlog')
        save(root/'retained-unknown-run.json',state);save(root/'retained-unknown-quota.json',quota)
        report['checks']['actual_expired_owner_and_higher_epoch_required_on_network_recovery']=after_owner[3]!=prior[3] and after_owner[4]>prior[4]
        report['checks']['recovery_preserves_unknown_attempt_and_blocks_zero_price_replay']=True
        report['recovery']={'samples':1,'expected_successful_task_completions':0,'safe_retained_budget_stops':1,'fail_closed_original_exit':victim.returncode,'original_pid':victim.pid,'successor_pid':successor.pid,'original_queue_owner_epoch':prior[3:5],'successor_queue_owner_epoch':after_owner[3:5],'network_cut_to_safe_stop_ms':(ended-cut)//1_000_000,'network_restored_to_safe_stop_ms':(ended-restored)//1_000_000,'model_dispatches_before_cut':1,'additional_dispatches_during_or_after_cut':0,'backlog_attempts':0,'proxy':proxy.snapshot()}
        final=snapshot('final')
        if scoped(final,'operational-faults')!=unknown_before or len(effect.read_text().splitlines())!=1:raise RuntimeError('reused unknown effects or accounting changed')
        if db.profile()!=before_profile:raise RuntimeError('schema/dataset changed; no migration permitted')
        report['checks']['unknown_effect_namespace_and_schema_dataset_unchanged']=True
        report['checks']['every_fixture_http_request_observed_durable_budget_and_quota_reservation']=len(records)==17 and all(entry['durable_in_flight_attempts']>=1 for entry in records)
        report['completed']=True
    except Exception as error:
        report['failures'].append(clean(error))
        try:snapshot('failure')
        except Exception:pass
    finally:
        for gate in gates.values():gate.set()
        for process,label in reversed(children):
            if process.poll() is None:
                try:stop(process,label)
                except Exception as error:report['failures'].append('owned cleanup: '+clean(error))
        for proxy in proxies:proxy.close()
        if server is not None:server.shutdown();server.server_close()
        if thread is not None:thread.join(timeout=5)
        for stream in streams:stream.close()
        report['failures'].extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
        report['status']='passed' if report['completed'] and not report['failures'] else 'failed';save(root/'fixture-requests.json',records)
        report['fixture_requests_sha256']=sha(root/'fixture-requests.json');report['logs']={path.relative_to(root).as_posix():sha(path) for path in root.rglob('*.log')};report['ops_sources']={name:sha(Path(__file__).parent/name) for name in ['network_capacity_drill.py','owned_tcp_proxy.py','rolling_rehearsal.py','binary_witness.py']};save(root/'report.json',report)
    print(json.dumps(report));raise SystemExit(0 if report['status']=='passed' else 1)


@contextmanager
def quota_observation_lock(db,namespace,root):
    # No owner/epoch UPDATE. Only retain a row lock long enough to photograph the
    # actual successor's already-committed lease before releasing normal work.
    error=(root/'quota-observation-lock.log').open('xb')
    process=subprocess.Popen(['docker','exec','--interactive',db.cid,'psql','-X','-qAt','-v','ON_ERROR_STOP=1','-U','taskloom_accept','-d',db.database],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=error)
    escaped=namespace.replace("'","''")
    statement="BEGIN; SELECT 1 FROM taskloom_records WHERE namespace='"+escaped+"' AND family='quota' AND object_id='generation' FOR UPDATE; SELECT '__TASKLOOM_LOCK_READY__';\n"
    process.stdin.write(statement.encode());process.stdin.flush();buffer=b'';deadline=time.monotonic()+10
    try:
        while b'__TASKLOOM_LOCK_READY__' not in buffer:
            if time.monotonic()>deadline or process.poll() is not None:raise RuntimeError('owned quota observation lock not acquired')
            ready,_,_=select.select([process.stdout],[],[],.1)
            if ready:buffer+=os.read(process.stdout.fileno(),4096)
        yield
    finally:
        if process.poll() is None:
            try:process.stdin.write(b'ROLLBACK;\n\\q\n');process.stdin.flush();process.stdin.close();process.wait(timeout=10)
            except (OSError,subprocess.TimeoutExpired):process.kill();process.wait(timeout=5)
        error.close()


if __name__=='__main__':main()
