"""Native API acceptance for SQLite and an explicitly owned local PostgreSQL.

SDK SQL stores only: the application API still uses file stores. No user/RBAC,
queue, throughput or task-quality acceptance. Generation is explicitly zero-
priced with cap 0. The long-output interruption is a recovery probe; truncation
remains visible, not a successful long-task quality score.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from urllib.parse import quote,urlsplit
import uuid


def save(path,value):path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def calls(log):return log.read_bytes().count(b'[Request received] Model:')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path);parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--example',type=Path,required=True);parser.add_argument('--postgres-fixture',type=Path,required=True)
    parser.add_argument('--model-log',type=Path,required=True)
    args=parser.parse_args();cfg=json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1','localhost','::1'} or not cfg.get('prices')
            or any(p['input_per_million'] or p['output_per_million'] for p in cfg['prices'])):
        raise ValueError('explicit zero-priced loopback model required')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False);example=args.example.resolve()
    shutil.copy2(__file__,root/'harness.py');cfg['output_tokens']=128;save(root/'native.json',cfg)
    credentials=json.loads((args.postgres_fixture/'credentials.json').read_text())
    public=json.loads((args.postgres_fixture/'public.json').read_text())
    pg=credentials['TASKLOOM_TEST_POSTGRES_URL'];endpoint=urlsplit(pg)
    if endpoint.hostname!='127.0.0.1' or endpoint.port!=public['loopback_port']:
        raise ValueError('PostgreSQL credential must match the owned loopback fixture')
    # Never copy or serialize the URL/password. Only the named environment is used.
    urls={'sqlite':'sqlite://'+quote(str(root/'runtime ü space.sqlite'),safe='')+'?mode=rwc&cache=private','postgres':pg}
    report={'version':1,'completed':False,'cases':[],'failures':[],
        'evidence_kind':'actual local native API through SDK SQL run/workflow/session stores, cold reads and killed-process generation recovery',
        'binary_sha256':hashlib.sha256(example.read_bytes()).hexdigest(),'postgres_public_provenance':public,
        'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'cap_micros':0,'output_allowance':128,
                'model_credential_environment':cfg['provider']['key_env'],'database_credential_environment':'TASKLOOM_DATABASE_URL',
                'excluded':'hardware/electricity/current-chat; embedding and tool services unused'},
        'limits':['SDK stores only; local app/CLI/UI backend selection, corpus/memory/skills/embedding ledgers, authentication, quotas and queue are unfinished',
            'namespaces are selected by the trusted host; cross-scope reads here do not prove authenticated multi-user authorization',
            'one actual native generation is killed per backend; first request response usage is unknown and retained as such',
            '128-token long-output probe can truncate; it is lifecycle evidence, not long-task quality or a competitor/cost/latency ranking']}
    started=time.monotonic()
    for backend,url in urls.items():
        output=root/backend;output.mkdir();project='native-'+uuid.uuid4().hex;tenant='native-database'
        environment=dict(os.environ,TASKLOOM_DATABASE_URL=url)
        result={'backend':backend,'project':project,'checks':{},'failures':[]}
        process=None;process_log=None
        def run(action,*,cold=False,other_project=None):
            env=dict(environment)
            if cold:env.pop(cfg['provider']['key_env'],None)
            command=[str(example),action,tenant,other_project or project]
            if action not in {'inspect','init'}:command.append(str(root/'native.json'))
            value=subprocess.run(command,capture_output=True,timeout=180,env=env)
            if value.returncode:raise RuntimeError('example '+action+' failed: '+value.stderr.decode(errors='replace')[:2000])
            data=json.loads(value.stdout);save(output/(action+('-cold' if cold else '')+'.json'),data);return data
        try:
            run('init',cold=True)
            completed=run('run');inspected=run('inspect',cold=True)
            result['checks']['cold_process_read_equals_completed_native_snapshot']=inspected==completed
            result['checks']['native_two_role_workflow_completed']=completed['workflow']['status']=='completed' and len(completed['workflow_children'])==2
            result['checks']['two_native_session_turns_preserve_history']=len(completed['session']['turns'])==2 and completed['session']['active'] is None and len(completed['session_children'])==2
            result['checks']['all_native_children_have_parent_bindings']=all(c.get('trace_binding') for c in [*completed['workflow_children'].values(),*completed['session_children'].values()])
            content=completed['workflow_children']['first']['report']['messages'][1]['content']
            result['checks']['large_integer_and_decimal_type_retained_in_native_input']='9007199254740993' in content and '"decimal":1.0' in content
            other=run('inspect',cold=True,other_project='other-'+uuid.uuid4().hex)
            result['checks']['other_namespace_has_no_workflow_or_session']=other['workflow'] is None and other['session'] is None and not other['workflow_children'] and not other['session_children']
            # Save the real same-scope cold read separately from the other-scope probe.
            save(output/'same-scope-cold.json',inspected);save(output/'other-scope-cold.json',other)
            result['native_completed_usage']={key:sum(c['report']['usage'][key] for c in [*completed['workflow_children'].values(),*completed['session_children'].values()])
                                              for key in ['requests','input_tokens','output_tokens','cost_micros']}
            baseline=calls(args.model_log)
            process_log=(output/'killed-process.log').open('wb')
            process=subprocess.Popen([str(example),'start-long',tenant,project,str(root/'native.json')],env=environment,stdout=process_log,stderr=process_log,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
            for _ in range(60):
                view=run('inspect',cold=True);state=view['long_run'];journal=view['long_budget_journal']
                if state and state['cursor']['type']=='model_in_flight' and journal and journal['in_flight_attempts']==1 and calls(args.model_log)>baseline:break
                if process.poll() is not None:raise RuntimeError('long-output process finished before observed in-flight API dispatch')
                time.sleep(0.05)
            else:raise RuntimeError('in-flight SQL journal/native request observation deadline reached')
            save(output/'immediately-before-kill.json',view)
            process.kill();process.wait(timeout=10);result['killed_owned_process_id']=process.pid;result['killed_exit_code']=process.returncode
            after=run('inspect',cold=True);save(output/'after-kill-before-resume.json',after)
            result['checks']['actual_crash_reservation_retained']=after['long_budget_journal']==journal and after['long_budget_journal']['in_flight_attempts']==1 and after['long_run']['cursor']['type']=='model_in_flight'
            # The lease uses database time; this bounded wait lets the killed
            # 600ms owner expire. No process-local unlock substitutes for TTL.
            time.sleep(0.8)
            resumed=run('resume-long');events=[e['kind']['type'] for e in resumed['report']['events']]
            result['checks']['expired_process_lease_reacquired_and_model_recovery_explicit']=events.count('recovered_model')==1
            result['checks']['unknown_generation_attempt_not_silently_erased']=resumed['report']['budget']['uncertain_attempts']==1
            result['checks']['recovery_reached_terminal_checkpoint']=resumed['status']=='finished'
            before_reentry=calls(args.model_log);replay=run('resume-long')
            result['checks']['terminal_reentry_unchanged_no_native_request']=replay==resumed and calls(args.model_log)==before_reentry
            result['recovery_stop_reason']=resumed['report']['stop_reason'];result['recovery_known_usage']=resumed['report']['usage']
            result['unknown_native_response_usage_attempts']=1
            result['observed_native_requests_after_crash_baseline']=calls(args.model_log)-baseline
        except Exception as error:result['failures'].append(str(error))
        finally:
            if process is not None and process.poll() is None:process.kill();process.wait(timeout=10)
            if process_log is not None:process_log.close()
        result['failures'].extend('check failed: '+name for name,passed in result['checks'].items() if not passed)
        result['status']='failed' if result['failures'] else 'passed';save(output/'report.json',result)
        report['cases'].append(result);save(root/'report.json',report)
        print(json.dumps(result,ensure_ascii=False),flush=True)
    report['completed']=True;report['failures']=[c['backend']+': '+failure for c in report['cases'] for failure in c['failures']]
    report.update(status='failed' if report['failures'] else 'passed',wall_ms=round((time.monotonic()-started)*1000))
    save(root/'report.json',report)
    raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
