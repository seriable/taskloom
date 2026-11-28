"""Native local API through SQL-backed Application/CLI/API and cold restarts.

Explicit zero-priced loopback generation only. This is bounded persistence/
lifecycle evidence, not long-task/semantic quality, queue, load or leadership.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from urllib.parse import quote,urlsplit
import uuid

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'benchmarks'))
from native_retrieval import host,save

def node(identity,task,after=None,params=None):
    return {'id':identity,'task':task,'depends_on':after or [],'condition':{'type':'always'},
        'allow_skipped_dependencies':False,'select':{},'params':params or {}}

def limits():
    return {'max_nodes':32,'max_parallel':2,'max_dispatches':64,'max_output_bytes':131072,'max_cost_micros':0,
        'max_duration':{'secs':300,'nanos':0},'shutdown_grace':{'secs':5,'nanos':0}}


def idle(api,kind,identity):
    for _ in range(1200):
        value=api('/'+kind+'/'+identity)
        if not value['job']['active']:
            if value['job']['error']:raise RuntimeError(value['job']['error'])
            return value
        time.sleep(0.05)
    raise RuntimeError('native application deadline reached')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path);parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--taskloom',type=Path,required=True);parser.add_argument('--admin',type=Path,required=True)
    parser.add_argument('--postgres-fixture',type=Path,required=True);parser.add_argument('--model-log',type=Path,required=True)
    args=parser.parse_args();cfg=json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1','localhost','::1'} or not cfg.get('prices')
            or any(p['input_per_million'] or p['output_per_million'] for p in cfg['prices'])):
        raise ValueError('explicit zero-priced loopback native configuration required')
    if not os.environ.get(cfg['provider']['key_env']):raise ValueError('configure named local model key environment first')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    executable=args.taskloom.resolve();admin=args.admin.resolve()
    public=json.loads((args.postgres_fixture/'public.json').read_text())
    pg=json.loads((args.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    parsed=urlsplit(pg)
    if parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:raise ValueError('owned loopback database required')
    requests=lambda:args.model_log.read_bytes().count(b'[Request received] Model:')
    report={'version':1,'completed':False,'cases':[],'failures':[],
        'scope':'actual native short Application/CLI/API flows over SQLite/PostgreSQL, cold reads and host restart',
        'taskloom_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'admin_sha256':hashlib.sha256(admin.read_bytes()).hexdigest(),
        'postgres_public_provenance':public,
        'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'cap_micros':0,'output_allowance':256,
            'model_credential_environment':cfg['provider']['key_env'],'database_credential_environment':'TASKLOOM_DATABASE_URL',
            'excluded':'hardware/electricity/current chat; no embedding/tool-service usage'},
        'limits':['Native short text, template and visible preference extraction; not held-out semantic/research/long-task quality.',
            'Corpus/package authoring and memory review are explicitly local host/operator actions, never model decisions.',
            'No native embedding in this campaign; SQL embedding admission has separately labelled HTTP fixture controls.',
            'Single-operator API here; multi-user SQL identities have separate transport/authority evidence.',
            'No legacy bulk importer, durable global quota/queue/workers, load or whole-goal completion.']}
    started=time.monotonic()
    for backend,url in {'sqlite':'sqlite://'+quote(str(root/'application ü.sqlite'),safe='')+'?mode=rwc&cache=private','postgres':pg}.items():
        out=root/backend;out.mkdir();project='app-'+uuid.uuid4().hex;tenant='native-app'
        os.environ['TASKLOOM_DATABASE_URL']=url
        result={'backend':backend,'project':project,'checks':{},'failures':[]};runs={}
        app_cfg=dict(cfg,state_dir=str(out/'unused-file-state'),output_tokens=256,knowledge=False,
            database={'url_env':'TASKLOOM_DATABASE_URL','tenant':tenant,'project':project,'lease_ms':600},
            system='Follow the user request briefly. No tools are available.',
            memory={'scope':{'tenant':tenant,'project':project,'owner':'alice'},'actor':'local-operator','curate':True},
            limits={'max_steps':4,'max_tool_calls':0,'max_duration':{'secs':300,'nanos':0},'tool_timeout':{'secs':5,'nanos':0},'max_tool_result_bytes':65536,'max_cost_micros':0},
            roles=[{'name':'writer','system':'Read NodeContext input.goal and summarize in one short sentence. No tools.','tools':[],'output':{'type':'text'}},
                {'name':'reviewer','system':'Read dependencies.first and review the summary in one sentence starting Reviewed:. No tools.','tools':[],'output':{'type':'text'}}])
        save(out/'app.json',app_cfg)
        def command(parts,cold=False):
            env=dict(os.environ)
            if cold:env.pop(cfg['provider']['key_env'],None)
            value=subprocess.run([str(executable),'--config',str(out/'app.json'),*map(str,parts)],env=env,capture_output=True,timeout=180)
            if value.returncode:raise RuntimeError('CLI '+str(parts[0])+' failed: '+value.stderr.decode(errors='replace')[:1800])
            return json.loads(value.stdout)
        def retain(state):runs[state['report']['run_id']]=state;return state
        try:
            migrated=command(['database-migrate'],cold=True);save(out/'migration.json',migrated)
            # Author a tool-less inert package. Installer independently checks all bytes.
            package=out/'package';package.mkdir()
            text='Read NodeContext input.goal. Return one brief sentence. No tools and no additional authority.'
            (package/'answer.md').write_text(text,encoding='utf-8')
            save(package/'skill.json',{'format':1,'name':'native-sql-answer','version':'1.0.0','description':'Explicit SQL native template',
                'files':{'answer.md':hashlib.sha256(text.encode()).hexdigest()},'roles':[{'name':'answer','instructions':'answer.md','tools':[],'output':{'type':'text'}}],
                'templates':[{'id':'answer','title':'Answer','description':'Bounded native SQL example','input_schema':{'type':'object'},'workflow':{'type':'dag','definition':{'nodes':[node('answer','answer')]}}}]})
            packed=subprocess.run([sys.executable,str(Path(__file__).resolve().parents[1]/'skills/pack.py'),str(package),str(out/'package.tar')],capture_output=True,timeout=30)
            if packed.returncode:raise RuntimeError('inert package authoring failed')
            receipt=command(['skill-install',out/'package.tar',0],cold=True);save(out/'skill-install.json',receipt)
            app_cfg['skills']=[{'package':receipt['package'],'mount':'sql','grant':{'tools':[],'permissions':[],'credentials':[]}}]
            save(out/'app.json',app_cfg);before=requests()
            with host(executable,out/'app.json',out) as api:
                api('/knowledge',{'document':{'id':'source','title':'Zephyr policy','source':'fixture://operator-source','text':'Retain audit records for thirty days. Unicode 原文 remains exact.'},'expected_revision':0})
                recalled=api('/knowledge/search',{'text':'Zephyr','top_k':5,'max_bytes':24000});save(out/'retrieval.json',recalled)
                result['checks']['sql_corpus_title_recall_and_exact_body']=bool(recalled['hits']) and 'Unicode 原文' in recalled['hits'][0]['chunk']['text']
                request={'id':'flow','input':{'goal':'Explain why approval binds exact operation input.'},'peer':None,
                    'definition':{'nodes':[node('first','writer',params={'large':9007199254740993,'decimal':1.0}),node('second','reviewer',['first'])]},
                    'limits':limits()}
                view=api('/workflow-design/preview',request);save(out/'preview.json',view)
                api('/workflow-design/launch',{'request':view['request'],'preview_key':view['preview_key']})
                flow=idle(api,'workflows','flow');save(out/'flow.json',flow)
                children=[retain(api('/runs/'+n['child']['run_id'])['state']) for n in flow['state']['nodes']]
                result['checks']['native_two_role_sql_workflow_completed']=flow['state']['status']=='completed' and len(children)==2 and all(s['report']['stop_reason']=='completed' for s in children)
                frozen=children[0]['report']['messages'][1]['content']
                result['checks']['native_input_preserves_large_integer_and_decimal']='9007199254740993' in frozen and '"decimal":1.0' in frozen
                api('/sessions',{'id':'source-session'});api('/sessions/source-session/turns',{'turn_id':'one','prompt':'I prefer English replies.'})
                source=idle(api,'sessions','source-session');save(out/'source.json',source);retain(api('/runs/'+source['session']['turns'][0]['binding']['run_id'])['state'])
                extraction=api('/memory/extract',{'expected_revision':0,'session_id':'source-session','turn_id':'one','request_id':'native-learn'});save(out/'extraction.json',extraction);retain(extraction['run'])
                if not extraction['proposals_recorded']:raise RuntimeError('native memory extraction did not complete')
                candidates=[r for r in extraction['memory']['records'].values() if r['kind']=='preference' and 'english' in r['value'].lower()]
                if len(candidates)!=1:raise RuntimeError('native model did not produce exactly one declared English preference')
                record=candidates[0];active=api('/memory/review',{'expected_revision':extraction['memory']['revision'],'id':record['id'],'accept':True,'note':'Explicit local operator verifies original source.'});save(out/'reviewed.json',active)
                api('/sessions',{'id':'memory-session'});api('/sessions/memory-session/memory-turns',{'turn_id':'one','prompt':'Which reply language did I request? Answer briefly.'})
                memory=idle(api,'sessions','memory-session');memory_run=retain(api('/runs/'+memory['session']['turns'][0]['binding']['run_id'])['state']);save(out/'memory-session.json',memory)
                result['checks']['native_memory_turn_injects_reviewed_record_without_tools']=record['id'] in memory['session']['turns'][0]['binding']['input'] and not memory_run['config']['tools'] and memory_run['report']['stop_reason']=='completed'
                api('/templates/sql/answer/runs',{'id':'template','input':{'goal':'State the exact-approval rule in one short sentence.'},'limits':limits()})
                template=idle(api,'workflows','template');save(out/'template.json',template);retain(api('/runs/'+template['state']['nodes'][0]['child']['run_id'])['state'])
                result['checks']['native_template_loaded_from_sql_package']=template['state']['status']=='completed' and bool(runs[template['state']['nodes'][0]['child']['run_id']]['config']['skill_binding'])
                trace=api('/diagnostics/workflow/flow');child_trace=api('/diagnostics/run/'+children[0]['report']['run_id']);save(out/'trace.json',trace);save(out/'child-trace.json',child_trace)
                result['checks']['sql_parent_and_standalone_child_trace_ids_match']=trace['snapshot']['trace_id']==child_trace['snapshot']['trace_id']
            cold=command(['inspect',children[0]['report']['run_id']],cold=True);save(out/'cold-child.json',cold)
            result['checks']['cold_cli_read_without_generation_credential']=cold==children[0]
            catalog=command(['skills'],cold=True);save(out/'cold-skills.json',catalog)
            result['checks']['cold_sql_skill_catalog_retains_pin']=catalog['entries'][receipt['package']['name']+'@1.0.0']['reference']==receipt['package']
            cold_memory=command(['memory'],cold=True);save(out/'cold-memory.json',cold_memory)
            result['checks']['cold_sql_memory_matches_operator_review']=cold_memory==active
            cli_run=retain(command(['run','cli-short','State the approval rule in one brief sentence.']));save(out/'cli-run.json',cli_run)
            terminal=command(['resume','cli-short']);save(out/'cli-terminal-reentry.json',terminal)
            result['checks']['cli_shutdown_drains_release_for_immediate_terminal_reentry']=terminal==cli_run
            stable=requests()
            with host(executable,out/'app.json',out) as api:
                again=api('/workflows/flow');save(out/'restarted-flow.json',again)
                api('/workflows/flow/resume',{});reentry=idle(api,'workflows','flow');save(out/'terminal-flow-reentry.json',reentry)
                result['checks']['cold_host_restart_and_terminal_reentry_keep_revision']=again['state']==flow['state']==reentry['state'] and requests()==stable
                result['checks']['cold_host_recall_retains_reviewed_preference']=api('/memory/recall',{'text':'language'})['hits'][0]['record']['id']==record['id']
            foreign=json.loads(json.dumps(app_cfg));foreign['database']['project']='foreign-'+uuid.uuid4().hex;foreign['memory']['scope']['project']=foreign['database']['project'];foreign['skills']=[]
            save(out/'foreign.json',foreign)
            value=subprocess.run([str(executable),'--config',str(out/'foreign.json'),'list'],env={k:v for k,v in os.environ.items() if k!=cfg['provider']['key_env']},capture_output=True,timeout=30)
            result['checks']['other_sql_namespace_empty_on_cold_cli']=value.returncode==0 and json.loads(value.stdout)==[]
            result['checks']['sql_mode_creates_no_file_journal_fallback']=not Path(app_cfg['state_dir']).exists()
            result['native_api_attempts_observed']=requests()-before
            result['native_known_usage']={key:sum(s['report']['usage'][key] for s in runs.values()) for key in ['requests','input_tokens','output_tokens','cost_micros']}
            result['checks']['native_requests_equal_unique_saved_generation_usage']=result['native_api_attempts_observed']==result['native_known_usage']['requests']
        except Exception as error:result['failures'].append(str(error))
        save(out/'native-runs.json',runs)
        result['failures'].extend('check failed: '+k for k,v in result['checks'].items() if not v)
        result['status']='failed' if result['failures'] else 'passed';save(out/'report.json',result)
        report['cases'].append(result);report['failures'].extend(backend+': '+v for v in result['failures'])
    report['completed']=True;report['status']='failed' if report['failures'] else 'passed';report['wall_ms']=round((time.monotonic()-started)*1000)
    save(root/'report.json',report);print(json.dumps(report,ensure_ascii=False));raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
