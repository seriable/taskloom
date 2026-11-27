"""Actual zero-fee local model acceptance of configuration/plan review and launch.

No UI, general task-quality, competitor, paid service or throughput claim. New
configuration proposals are reviewed only; the current host stays unchanged.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from urllib.parse import urlsplit

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'benchmarks'))
from native_retrieval import host,save


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--taskloom',type=Path,required=True)
    args=parser.parse_args()
    cfg=json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1','localhost','::1'}
            or not cfg.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in cfg['prices'])):
        raise ValueError('explicitly zero-priced loopback model required')
    root=args.out.resolve(); root.mkdir(parents=True,exist_ok=False)
    executable=args.taskloom.resolve()
    cfg.update(state_dir=str(root/'state'),knowledge=False,mcp=[],skills=[],memory=None,observability=None,
        research={},retrieval={'mode':'lexical'},sandbox=None,sandbox_top_level=False,final_output=None,
        roles=[{'name':'worker','system':'Read the supplied NodeContext as task data. Summarize its task and parameters in one short sentence. No tools are available.',
                'tools':[],'output':{'type':'text'}}])
    subprocess.run([str(executable),'init',str(root/'defaults.json')],capture_output=True,check=True)
    defaults=json.loads((root/'defaults.json').read_text(encoding='utf-8'))
    cfg['limits']=dict(defaults['limits'],max_cost_micros=0,max_steps=4)
    save(root/'taskloom.json',cfg)
    shutil.copy2(__file__,root/'harness.py')
    shutil.copy2(Path(__file__).resolve().parents[1]/'benchmarks/native_retrieval.py',root/'native_retrieval.py')
    report={'version':1,'checks':{},'failures':[],
        'evidence_kind':'actual local native API + configuration static review + exact workflow preview/launch',
        'binary_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),
        'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'generation_cap_micros':0,
                'credential_environment':cfg['provider']['key_env'],'excluded':'hardware/electricity/current-chat; embedding and external tools unused'},
        'limitations':['code/API integration, no browser or complete P2 experience acceptance',
                       'configuration review is static; no new service, skill, credential or executable is activated',
                       'single-operator file host, not multi-user/database/worker-queue acceptance']}
    started=time.monotonic()
    try:
        with host(executable,root/'taskloom.json',root) as api:
            original=api('/setup'); save(root/'setup-before.json',original)
            proposal=dict(cfg,mcp=[{'endpoint':'http://127.0.0.1:1/not-contacted','token_env':'TASKLOOM_PLANNING_INTENTIONALLY_ABSENT',
                'namespace':'proposed-only','bindings':[{'name':'proposed_read','remote_name':'read','effect':'read_only','permissions':['proposed:read']}]}])
            checked=api('/setup/validate',proposal); save(root/'proposed-review.json',checked)
            after=api('/setup')
            report['checks']['static_proposal_does_not_activate_tools_or_change_host']=original==after and not checked['services_contacted'] and not checked['configuration_applied']
            request={'id':'native-preview','input':{'goal':'Explain exact approvals briefly.'},'peer':None,
                'definition':{'nodes':[{'id':'work','task':'worker','depends_on':[],'condition':{'type':'always'},
                    'allow_skipped_dependencies':False,'select':{},'params':{'decimal':1.0,'large':9007199254740993}}]},
                'limits':{'max_nodes':32,'max_parallel':2,'max_dispatches':64,'max_output_bytes':131072,'max_cost_micros':0,
                    'max_duration':{'secs':300,'nanos':0},'shutdown_grace':{'secs':5,'nanos':0}}}
            view=api('/workflow-design/preview',request); save(root/'preview.json',view)
            report['checks']['preview_has_no_dispatch_or_id_reservation']=not view['dispatch_performed'] and not view['identity_reserved'] and view['plan']['dispatches']==0 and not view['plan']['events']
            report['checks']['preview_does_not_persist_workflow']=not api('/workflows')['workflows']
            edited=json.loads(json.dumps(view['request'])); edited['input']['goal']='different input'
            try:
                api('/workflow-design/launch',{'request':edited,'preview_key':view['preview_key']})
                report['checks']['changed_input_refused_before_dispatch']=False
            except RuntimeError as error:
                report['checks']['changed_input_refused_before_dispatch']=str(error).startswith('HTTP 409:') and not api('/workflows')['workflows']
            api('/workflow-design/launch',{'request':view['request'],'preview_key':view['preview_key']})
            for _ in range(1200):
                observed=api('/workflows/native-preview')
                if not observed['job']['active']: break
                time.sleep(0.1)
            else: raise RuntimeError('native workflow acceptance deadline reached')
            save(root/'workflow.json',observed)
            state=observed['state']; child=api('/runs/'+state['nodes'][0]['child']['run_id'])['state']; save(root/'child.json',child)
            report['checks']['actual_native_generation_completed']=state['status']=='completed' and child['report']['usage']['requests']>=1 and child['report']['stop_reason']=='completed'
            report['checks']['frozen_input_definition_and_limits_equal_reviewed_plan']=state['input']==view['plan']['input'] and state['definition']==view['plan']['definition'] and state['limits']==view['plan']['limits']
            frozen=child['report']['messages'][1]['content']
            report['checks']['large_integer_and_float_type_retained_in_model_input']='9007199254740993' in frozen and '"decimal":1.0' in frozen
            report['checks']['native_child_has_host_trace_binding']=child.get('trace_binding')=={'type':'workflow','workflow_id':'native-preview','node_id':'work'}
            report['native_usage']=child['report']['usage']; report['raw_native_final']=child['report']['messages'][-1]
        environment=dict(os.environ); environment.pop(cfg['provider']['key_env'],None); environment.pop('TASKLOOM_PLANNING_INTENTIONALLY_ABSENT',None)
        cold=subprocess.run([str(executable),'--config','not-needed-main.json','config-check',str(root/'taskloom.json')],
            capture_output=True,timeout=30,env=environment)
        if cold.returncode: raise RuntimeError(cold.stderr.decode(errors='replace'))
        cold_view=json.loads(cold.stdout); save(root/'cold-config-review.json',cold_view)
        report['checks']['cold_cli_reviews_without_model_credential_or_main_config']=not cold_view['services_contacted'] and not cold_view['configuration_applied']
    except Exception as error:
        report['failures'].append(str(error))
    report['failures'].extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
    report.update(status='failed' if report['failures'] else 'passed',wall_ms=round((time.monotonic()-started)*1000))
    save(root/'report.json',report)
    print(json.dumps(report,ensure_ascii=False))
    raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__': main()
