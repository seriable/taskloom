"""Bounded real SQL/API/process load, using the explicit offline fixture model.

No inference benchmark or production SLO. Four submitters, four worker processes,
48 independent runs, a paused/restarted control run and one unexecuted cold
scheduling retirement per real backend.
"""
import argparse
import concurrent.futures
import contextlib
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import time
from urllib.error import HTTPError,URLError
from urllib.parse import quote,urlsplit
from urllib.request import Request,build_opener,ProxyHandler
import uuid


def save(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def distribution(values):
    values=sorted(values)
    return {'samples':len(values),'min_ms':values[0],'p50_ms':values[(len(values)-1)//2],'p95_ms':values[(95*len(values)+99)//100-1],'max_ms':values[-1]}
def peak(intervals):
    # End before start at the same millisecond: do not invent sub-ms overlap.
    events=sorted([(start,1) for start,end in intervals]+[(end,-1) for start,end in intervals])
    active=maximum=0
    for _,delta in events:active+=delta;maximum=max(maximum,active)
    return maximum


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('out',type=Path);p.add_argument('--taskloom',type=Path,required=True);p.add_argument('--admin',type=Path,required=True);p.add_argument('--postgres-fixture',type=Path,required=True)
    a=p.parse_args();root=a.out.resolve();root.mkdir(parents=True,exist_ok=False);exe=a.taskloom.resolve();admin=a.admin.resolve()
    public=json.loads((a.postgres_fixture/'public.json').read_text());pg=json.loads((a.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL'];parsed=urlsplit(pg)
    if parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:raise ValueError('owned loopback PostgreSQL required')
    report={'version':1,'completed':False,'cases':[],'failures':[],'scope':__doc__,'model':'explicit offline demonstration fixture; zero external generation/embedding/tool requests',
        'workload':{'producer_threads':4,'worker_processes':4,'healthy_runs':48,'restart_control_runs':1,'max_open_jobs':24,'max_running':4,'production_lease_ms':10000},
        'taskloom_sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'admin_sha256':hashlib.sha256(admin.read_bytes()).hexdigest(),'postgres_public_provenance':public,
        'hardware':{'os':platform.platform(),'processor':platform.processor(),'logical_cpus':os.cpu_count()},
        'limits':['Finite visible workload on one local host; no native inference/throughput comparison, production SLO or unbounded scalability claim.',
            'Latency uses HTTP client monotonic timers and persisted database event clocks; includes SQLite/PostgreSQL transactions and polling/process overhead.',
            'Process-death uncertain effects have a separate actual native campaign, not a fixture load success label.',
            'Private bearer/URL files remain local ignored artifacts; the host explicitly authorizes worker configs and roles.']}
    for backend,url in {'sqlite':'sqlite://'+quote(str(root/'load.sqlite'),safe='')+'?mode=rwc&cache=private','postgres':pg}.items():
        out=root/backend;out.mkdir();tenant='queue-load';project=uuid.uuid4().hex;foreign=uuid.uuid4().hex;env=dict(os.environ,TASKLOOM_DATABASE_URL=url)
        case={'backend':backend,'tenant':tenant,'project':project,'checks':{},'failures':[]};workers=[];logs=[]
        def cli(parts,config='app.json'):
            value=subprocess.run([str(exe),'--config',str(out/config),*map(str,parts)],env=env,capture_output=True,timeout=90)
            if value.returncode:raise RuntimeError('CLI '+str(parts[0])+' failed: '+value.stderr.decode(errors='replace')[:1000])
            return json.loads(value.stdout)
        def administrative(parts):
            value=subprocess.run([str(admin),*map(str,parts)],env=env,capture_output=True,timeout=60)
            if value.returncode:raise RuntimeError('local administration '+str(parts[0])+' failed: '+value.stderr.decode(errors='replace')[:1000])
            return value.stdout
        @contextlib.contextmanager
        def service(name):
            with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
            save(out/(name+'.json'),{'listen':'127.0.0.1:'+str(port),'projects':[{'tenant':tenant,'project':project,'config':str(out/'app.json')},{'tenant':tenant,'project':foreign,'config':str(out/'foreign.json')}]})
            log=(out/(name+'.log')).open('wb');process=subprocess.Popen([str(admin),'serve',str(out/(name+'.json'))],env=env,stdout=log,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
            base=f'http://127.0.0.1:{port}/tenants/{tenant}/projects/{project}/api';opener=build_opener(ProxyHandler({}))
            def api(path,body=None,operator=False,foreign_scope=False):
                bearer=operator_token if operator else runner_token
                endpoint=base.replace(project,foreign) if foreign_scope else base
                request=Request(endpoint+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+bearer,'Content-Type':'application/json'})
                with opener.open(request,timeout=30) as response:return json.loads(response.read(16*1024*1024))
            try:
                for _ in range(100):
                    if process.poll() is not None:raise RuntimeError('owned service process exited')
                    try:api('/identity');break
                    except URLError:time.sleep(0.1)
                else:raise RuntimeError('owned service readiness deadline')
                yield api
            finally:
                process.terminate()
                try:process.wait(timeout=10)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=10)
                log.close()
        try:
            subprocess.run([str(exe),'init',str(out/'defaults.json')],capture_output=True,check=True)
            defaults=json.loads((out/'defaults.json').read_text());config=dict(defaults,state_dir=str(out/'state'),provider={'type':'offline'},knowledge=False,limits=dict(defaults['limits'],max_cost_micros=0),database={'url_env':'TASKLOOM_DATABASE_URL','tenant':tenant,'project':project})
            save(out/'app.json',config);save(out/'foreign.json',dict(config,state_dir=str(out/'foreign-state'),database={'url_env':'TASKLOOM_DATABASE_URL','tenant':tenant,'project':foreign}))
            save(out/'migration.json',cli(['database-migrate']));save(out/'limits.json',{'max_open_jobs':24,'max_running':4});save(out/'policy.json',cli(['queue-configure',out/'limits.json','0','load-admin','explicit finite fixture load']))
            cli(['queue-configure',out/'limits.json','0','load-admin','separate foreign fixture scope'],config='foreign.json')
            save(out/'foreign-task.json',{'type':'run','id':'hidden','prompt':'foreign offline fixture'});cli(['queue-submit',out/'foreign-task.json','foreign-local-operator'],config='foreign.json')
            catalog=json.loads(administrative(['catalog']));revision=catalog['revision'];users={}
            for role in ['runner','operator']:
                user=role+'-'+uuid.uuid4().hex;users[role]=user;path=out/(role+'.user.json');save(path,{'id':user,'enabled':True,'memberships':[{'tenant':tenant,'project':project,'role':role}]})
                administrative(['put-user',path,str(revision),'load-admin']);revision+=1
                receipt=json.loads(administrative(['issue',user,out/(role+'.bearer'),'load-admin']));users[role+'-token-id']=receipt['token_id'];revision+=1
            runner_token=(out/'runner.bearer').read_text();operator_token=(out/'operator.bearer').read_text()
            save(out/'generation-limits.json',{'currency':'USD','max_cost_micros':0,'max_outstanding_attempts':4});administrative(['quota-set',tenant,project,out/'generation-limits.json','0','load-admin','explicit offline fixture reservation cap'])
            start=time.monotonic();enqueue_latencies=[];retry_counts=[];peak_running=peak_open=0;paused=None
            with service('service-before-restart') as api:
                def submit(index):
                    body={'task':{'type':'run','id':f'job-{index:03}','prompt':'offline load fixture '+str(index)},'actor':'spoofed-admin'};attempts=0;started=time.monotonic()
                    for _ in range(600):
                        try:
                            result=api('/queue/jobs',body)['job']
                            if result['submitted_by']!=users['runner']:raise ValueError('service did not bind actual authenticated submitter')
                            return round((time.monotonic()-started)*1000),attempts,result['id']
                        except HTTPError as error:
                            detail=error.read(8192).decode();attempts+=1
                            if error.code!=409 or 'open-job capacity reached' not in detail:raise RuntimeError('unexpected enqueue status '+str(error.code)+': '+detail)
                            time.sleep(0.05)
                    raise RuntimeError('explicit finite enqueue deadline exceeded')
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:accepted=list(pool.map(submit,range(24)))
                initial=api('/queue/jobs')['jobs'];peak_open=sum(job['state'] not in ['completed','stopped'] for job in initial)
                try:api('/queue/jobs',{'task':{'type':'run','id':'known-over-cap','prompt':'offline fixture'},'actor':'spoofed-admin'})
                except HTTPError as error:case['checks']['known_capacity_refusal_before_workers_start']=error.code==409
                else:case['checks']['known_capacity_refusal_before_workers_start']=False
                try:api('/runs',{'id':'direct-bypass','prompt':'offline fixture'})
                except HTTPError as error:case['checks']['remote_runner_cannot_bypass_queue_admission']=error.code==409
                else:case['checks']['remote_runner_cannot_bypass_queue_admission']=False
                try:api('/queue/jobs/run-hidden',foreign_scope=True)
                except HTTPError as error:case['checks']['known_foreign_pending_job_refuses_scope_access']=error.code==403
                else:case['checks']['known_foreign_pending_job_refuses_scope_access']=False
                for index in range(4):
                    log=(out/f'worker-{index}.log').open('wb');logs.append(log);workers.append(subprocess.Popen([str(exe),'--config',str(out/'app.json'),'worker',f'load-worker-{index}'],env=env,stdout=log,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0))
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:accepted.extend(pool.map(submit,range(24,48)))
                for _ in range(2400):
                    jobs=api('/queue/jobs')['jobs'];peak_running=max(peak_running,sum(job['state']=='running' for job in jobs));peak_open=max(peak_open,sum(job['state'] not in ['completed','stopped'] for job in jobs))
                    if any(process.poll() is not None for process in workers):raise RuntimeError('owned load worker exited unexpectedly')
                    if len(jobs)==48 and all(job['state']=='completed' for job in jobs):break
                    time.sleep(0.05)
                else:raise RuntimeError('finite queue drain deadline exceeded')
                healthy_wall=time.monotonic()-start;save(out/'healthy-jobs.json',jobs)
                case['checks']['all_48_unique_jobs_complete_once']=len({job['id'] for job in jobs})==48 and all(job['attempts']==1 for job in jobs)
                case['checks']['observed_running_and_open_counts_within_caps']=peak_running<=4 and peak_open<=24
                case['checks']['all_workers_remain_live_during_drain']=all(process.poll() is None for process in workers)
                # Pause before a worker can claim by stopping the current four
                # now-idle owned processes, then restart them only after pause.
                for process in workers:process.terminate();process.wait(timeout=10)
                workers=[]
                paused=api('/queue/jobs',{'task':{'type':'run','id':'restart-control','prompt':'offline paused fixture'},'actor':'spoofed-admin'})['job']
                paused=api('/queue/jobs/'+paused['id']+'/control',{'expected_revision':paused['revision'],'action':'pause','actor':'spoofed-admin','note':'persist across actual service process restart'},operator=True)['job'];save(out/'paused-before-restart.json',paused)
                case['checks']['control_actor_bound_to_authenticated_operator']=paused['control']['actor']==users['operator']
            with service('service-after-restart') as api:
                restored=api('/queue/jobs/'+paused['id'])['job'];save(out/'paused-after-restart.json',restored);case['checks']['actual_service_restart_retains_exact_paused_job']=restored==paused
                api('/queue/jobs/'+paused['id']+'/resume',{'expected_revision':restored['revision'],'actor':'spoofed-admin','note':'explicit continuation after service restart'})
                completed=cli(['worker-once','restart-worker']);save(out/'restart-worker.json',completed);case['checks']['separate_worker_completes_explicit_post_restart_resume']=completed['state']=='completed' and completed['attempts']==1
                administrative(['revoke',users['runner-token-id'],'load-admin'])
                try:api('/queue')
                except HTTPError as error:case['checks']['live_token_revocation_applies_after_restart']=error.code==401
                else:case['checks']['live_token_revocation_applies_after_restart']=False
            save(out/'cold-task.json',{'type':'run','id':'cold-retire','prompt':'Never execute this fixture; retire its scheduling explicitly.'});unstarted=cli(['queue-submit',out/'cold-task.json','cold-local-operator'])
            unavailable=dict(config,provider={'type':'openai','base_url':'http://127.0.0.1:1/v1','key_env':'TASKLOOM_NEVER_CONFIGURED_QUEUE_MODEL','namespace':'unavailable-new-host'},mcp=[{'endpoint':'http://127.0.0.1:1/mcp','token_env':'TASKLOOM_NEVER_CONFIGURED_QUEUE_CONNECTOR','namespace':'unavailable-new-connector','bindings':[]}])
            env.pop('TASKLOOM_NEVER_CONFIGURED_QUEUE_MODEL',None);env.pop('TASKLOOM_NEVER_CONFIGURED_QUEUE_CONNECTOR',None);save(out/'unavailable-services.json',unavailable)
            retired=cli(['queue-control',unstarted['id'],str(unstarted['revision']),'abandon','cold-local-operator','credentials unavailable; stop only scheduling, retain original runtime and spending'],config='unavailable-services.json');save(out/'cold-retired.json',retired)
            case['checks']['cold_cli_retires_scheduling_without_model_or_connector_credentials']=retired['state']=='abandoned' and retired['payload']==unstarted['payload'] and cli(['queue-inspect',retired['id']],config='unavailable-services.json')['runtime_bindings']==[]
            cold=cli(['queue']);save(out/'cold-queue.json',cold);foreign_state=cli(['queue'],config='foreign.json');save(out/'foreign-queue.json',foreign_state)
            case['checks']['local_workers_never_consume_other_project']=len(foreign_state['jobs'])==1 and foreign_state['jobs'][0]['state']=='pending' and foreign_state['jobs'][0]['attempts']==0
            ledger=json.loads(administrative(['quota-get',tenant,project]));save(out/'generation-quota.json',ledger);case['checks']['49_settled_fixture_attempts_no_uncertainty_or_charges']=ledger['usage']['runs']==49 and ledger['usage']['committed_micros']==0 and ledger['usage']['in_flight_attempts']==0 and ledger['usage']['uncertain_attempts']==0
            for latency,retries,_ in accepted:enqueue_latencies.append(latency);retry_counts.append(retries)
            wait=[];execution=[];total=[];running_intervals=[];open_intervals=[]
            for job in jobs:
                submitted=next(event['unix_ms'] for event in job['events'] if event['action']=='enqueue');claimed=next(event['unix_ms'] for event in job['events'] if event['action']=='claim');finished=next(event['unix_ms'] for event in job['events'] if event['action']=='finish')
                if not submitted<=claimed<=finished:raise ValueError('database wall clock moved backwards; timing is not comparable')
                wait.append(claimed-submitted);execution.append(finished-claimed);total.append(finished-submitted)
                running_intervals.append((claimed,finished));open_intervals.append((submitted,finished))
            case['checks']['persisted_event_intervals_respect_running_and_open_caps']=peak(running_intervals)<=4 and peak(open_intervals)<=24
            case['measurements']={'healthy_end_to_end_wall_ms':round(healthy_wall*1000),'healthy_fixture_jobs_per_second_including_admission_and_process_start':48/healthy_wall,'observed_peak_running':peak_running,'observed_peak_open':peak_open,'persisted_event_peak_running':peak(running_intervals),'persisted_event_peak_open':peak(open_intervals),'actual_worker_labels':sorted({job['worker'] for job in jobs}),'explicit_capacity_retries':sum(retry_counts),'enqueue_http_including_explicit_capacity_wait':distribution(enqueue_latencies),'queue_wait':distribution(wait),'execution_sql_and_fixture':distribution(execution),'enqueue_to_finish':distribution(total)}
        except Exception as error:case['failures'].append(str(error))
        finally:
            for process in workers:
                if process.poll() is None:process.terminate();process.wait(timeout=10)
            for log in logs:log.close()
        case['failures'].extend('check failed: '+key for key,passed in case['checks'].items() if not passed);case['status']='failed' if case['failures'] else 'passed';save(out/'report.json',case);report['cases'].append(case);report['failures'].extend(backend+': '+failure for failure in case['failures']);save(root/'report.json',report);print(json.dumps(case),flush=True)
    report['completed']=True;report['status']='failed' if report['failures'] else 'passed';save(root/'report.json',report);raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
