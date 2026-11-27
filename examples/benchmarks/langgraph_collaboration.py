"""Qualified actual LangGraph DAG/Send/conditional-review native comparison.

Framework graph scheduling is real LangGraph. Shared NativeRoles adapts only
model/knowledge I/O, bounded repairs and exact SDK provenance checks. All three
frozen V5 task goals/model/corpus/role guidance/output and zero-fee rules match;
opaque IDs, wrappers, repair diagnostics/retries and persistence differ. No
scripted model answers, process-death guarantee or superiority ranking.
"""
import argparse
import base64
import hashlib
import importlib.metadata
import inspect
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
from typing import Annotated,TypedDict

for name in ['LANGSMITH_TRACING','LANGCHAIN_TRACING','LANGCHAIN_TRACING_V2']:
    os.environ[name]='false'
os.environ['PYTHON_DOTENV_DISABLED']='1'
os.environ['OTEL_SDK_DISABLED']='true'

from langgraph.graph import StateGraph,START,END
from langgraph.types import Send
from native_retrieval import host,save
from native_role_protocol import NativeRoles


def merge_outputs(left,right):
    # No branch may silently overwrite another branch's handoff.
    if set(left)&set(right): raise ValueError('duplicate native node output')
    return dict(left,**right)


class CollaborationState(TypedDict):
    outputs:Annotated[dict,merge_outputs]


def build_graph(case,protocol):
    graph=StateGraph(CollaborationState)
    if case['kind']=='dag':
        names=[]
        for branch in case['branches']:
            identity=branch['id'];names.append(identity)
            def analyst(state,branch=branch):
                return {'outputs':{branch['id']:protocol.role(case,branch['id'],'analyst',params={'instructions':branch['instructions']})}}
            graph.add_node(identity,analyst);graph.add_edge(START,identity)
        if case.get('initial_draft'):
            identity='review-initial';names.append(identity)
            graph.add_node(identity,lambda state:{'outputs':{'review-initial':protocol.role(case,'review-initial','reviewer',params={'candidate':case['initial_draft']})}})
            graph.add_edge(START,identity)
        graph.add_node('draft',lambda state:{'outputs':{'draft':protocol.role(case,'draft','writer',dependencies={name:state['outputs'][name] for name in names})}})
        graph.add_edge(names,'draft')
        graph.add_node('review',lambda state:{'outputs':{'review':protocol.role(case,'review','reviewer',dependencies={'draft':state['outputs']['draft']})}})
        graph.add_edge('draft','review');graph.add_edge('review',END)
    elif case['kind']=='peer':
        graph.add_node('raw-plan',lambda state:{'outputs':{'raw-plan':protocol.role(case,'raw-plan','planner')}})
        graph.add_edge(START,'raw-plan')
        def validate_plan(state):
            tasks=state['outputs']['raw-plan']['tasks']
            if not 1<=len(tasks)<=2 or len({t['id'] for t in tasks})!=len(tasks):
                raise ValueError('native plan is outside the shared two-task bound')
            for task in tasks:
                if not re.fullmatch('[A-Za-z0-9_-]{1,64}',task['id']) or not task['instructions'].strip():
                    raise ValueError('invalid native plan task identity/instructions')
            # Match typed planning's absent optional input materialization.
            return {'outputs':{'plan':{'tasks':[dict(t,input=t.get('input')) for t in tasks]}}}
        graph.add_node('plan',validate_plan);graph.add_edge('raw-plan','plan')
        graph.add_conditional_edges('plan',lambda state:[Send('analyst',{'index':i,'task':task}) for i,task in enumerate(state['outputs']['plan']['tasks'])])
        def analyst(state):
            identity='worker-'+str(state['index'])
            return {'outputs':{identity:protocol.role(case,identity,'analyst',dependencies={'task':state['task']},params={'index':state['index']})}}
        graph.add_node('analyst',analyst)
        def writer(state,revision):
            outputs=state['outputs'];deps={name:output for name,output in outputs.items() if name.startswith('worker-')}
            deps['plan']=outputs['plan']
            if revision:
                deps['draft-0']=outputs['draft-0'];deps['check-review-0']=outputs['review-0']
            identity='draft-'+str(revision)
            return {'outputs':{identity:protocol.role(case,identity,'writer',dependencies=deps,params={'revision':revision})}}
        def reviewer(state,revision):
            identity='review-'+str(revision)
            return {'outputs':{identity:protocol.role(case,identity,'reviewer',dependencies={'draft':state['outputs']['draft-'+str(revision)]},params={'revision':revision})}}
        graph.add_node('draft-0',lambda state:writer(state,0));graph.add_edge('analyst','draft-0')
        graph.add_node('review-0',lambda state:reviewer(state,0));graph.add_edge('draft-0','review-0')
        graph.add_node('draft-1',lambda state:writer(state,1))
        graph.add_conditional_edges('review-0',lambda state:END if state['outputs']['review-0']['approved'] else 'draft-1')
        graph.add_node('review-1',lambda state:reviewer(state,1));graph.add_edge('draft-1','review-1');graph.add_edge('review-1',END)
    else:raise ValueError('unsupported suite kind')
    return graph.compile()


def assess(case,outputs,protocol,verifier):
    failures=[]
    if case['kind']=='peer':
        revision=1 if 'draft-1' in outputs else 0
        final=outputs['draft-'+str(revision)];review=outputs['review-'+str(revision)]
        rework=revision==1
    else:
        final=outputs['draft'];review=outputs['review']
        initial=outputs.get('review-initial',{})
        rework=initial.get('approved') is False and bool(initial.get('feedback','').strip())
    if review.get('approved') is not True:failures.append('native reviewer did not approve within the shared revision bound')
    if case['require_parallel'] and protocol.maximum_concurrent_analysts<2:failures.append('two actual analyst role lifecycles did not overlap')
    if case['require_rework'] and not rework:failures.append('required reviewer rejection/rewrite not exercised')
    for phrase in case['required_phrases']:
        if phrase.lower() not in final['text'].lower():failures.append('missing requested topic: '+phrase)
    evidence=[m['result']['output'] for record in protocol.records if record['role']=='writer' and record['status']=='completed'
              for m in record['messages'] if m['role']=='tool' and not m['result']['is_error']]
    checked=subprocess.run([str(verifier)],input=json.dumps({'report':final,'evidence':evidence}).encode(),capture_output=True,timeout=10)
    if checked.returncode:raise RuntimeError('shared SDK citation verification: '+checked.stderr.decode(errors='replace')[:1500])
    sources=json.loads(checked.stdout)['sources']
    if not set(case['required_documents'])<={s['document_id'] for s in sources}:failures.append('required source coverage missing')
    return {'final_output':final,'final_review':review,'review_rework_exercised':rework,'sources':sources,'failures':failures}


def control_code():
    distribution=importlib.metadata.distribution('langgraph')
    files={Path(distribution.locate_file(p)).resolve():p for p in distribution.files}
    observations=[]
    from langgraph.pregel import Pregel
    for cls in [StateGraph,Pregel]:
        path=Path(inspect.getfile(cls)).resolve();data=path.read_bytes();entry=files[path]
        observed=base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip('=')
        if not entry.hash or entry.hash.mode!='sha256' or entry.hash.value!=observed:
            raise ValueError('installed LangGraph control code differs from distribution RECORD')
        observations.append({'class':cls.__name__,'module':cls.__module__,'relative_path':str(entry).replace('\\','/'),
                             'sha256':hashlib.sha256(data).hexdigest(),'matches_distribution_record':True})
    return observations


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference',type=Path);parser.add_argument('out',type=Path)
    parser.add_argument('--taskloom',type=Path,required=True);parser.add_argument('--verifier',type=Path,required=True)
    args=parser.parse_args()
    reference=json.loads((args.reference/'report.json').read_text(encoding='utf-8'))
    suite=json.loads((args.reference/'suite.json').read_text(encoding='utf-8'))
    if not reference['completed'] or suite['version']!=5:raise ValueError('completed V5 native reference required')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    for name in ['langgraph_collaboration.py','native_role_protocol.py','native_retrieval.py']:
        shutil.copy2(Path(__file__).with_name(name),root/name)
    cfg=dict(reference['configuration'],state_dir=str(root/'state'),provider={'type':'offline'},prices=[],roles=[],skills=[],
             memory=None,mcp=[],observability=None,research={},sandbox=None,sandbox_top_level=False)
    save(root/'taskloom.json',cfg)
    report={'version':1,'framework':'LangGraph','framework_version':importlib.metadata.version('langgraph'),'python_version':sys.version,
        'upstream_control_code':control_code(),'completed':False,'cases':[],'reference_suite_sha256':reference['suite_sha256'],
        'evidence_kind':'actual StateGraph parallel DAG/Send analyst fan-out/conditional native review, shared bounded NativeRoles I/O',
        'conditions':{'model':reference['configuration']['model'],'output_tokens':reference['configuration']['output_tokens'],
            'same_suite_goals_corpus_and_role_prompts':True,'case_seconds':300,'per_role_steps':16,'per_role_requested_tools':32,
            'max_parallel':'same per-case 2 or 3; StateGraph RunnableConfig max_concurrency',
            'currency':'USD','input_per_million':0,'output_per_million':0,'campaign_cap_micros':1,
            'credential_environment':reference['configuration']['provider']['key_env']},
        'differences':['explicit model/tool I/O adapter, not prebuilt LangGraph agent or ecosystem integrations',
            'opaque run IDs, repaired-output feedback, provider envelope fields and transient HTTP retry behavior differ',
            'typed plan optional inputs materialize as null, matching the reference; no invented plan/role output',
            'no checkpoint saver/production spending store/approval/effect recovery configured in this adapter',
            'shared read-only Taskloom retrieval is not framework-native retrieval quality; no identical-conditioning or speed/leadership ranking'],
        'not_comparable':[],'unverified':['long tasks, multi-agent process-death, uncertain effects, independent factual quality'],
        'execution_provenance':{'taskloom_sha256':hashlib.sha256(args.taskloom.read_bytes()).hexdigest(),
            'verifier_sha256':hashlib.sha256(args.verifier.read_bytes()).hexdigest(),
            'harness_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            'role_adapter_sha256':hashlib.sha256(Path(__file__).with_name('native_role_protocol.py').read_bytes()).hexdigest()},
        'installed_versions':subprocess.check_output([sys.executable,'-m','pip','freeze'],text=True).splitlines(),
        'fees_excluded':'hardware/electricity/current-chat; embedding and remote tool services unused'}
    save(root/'report.json',report)
    with host(args.taskloom.resolve(),root/'taskloom.json',root) as api:
        for i,document in enumerate(suite['documents']):api('/knowledge',{'expected_revision':i,'document':document})
        for case in suite['cases']:
            directory=root/case['id'];directory.mkdir()
            protocol=NativeRoles(reference['configuration'],suite,directory,api,args.verifier.resolve(),reference)
            result={'id':case['id'],'kind':case['kind'],'failures':[]}
            started=time.monotonic()
            try:
                outputs=build_graph(case,protocol).invoke({'outputs':{}},{'max_concurrency':case.get('max_parallel',2),'recursion_limit':32})['outputs']
                result['node_outputs']=outputs
                result.update(assess(case,outputs,protocol,args.verifier.resolve()))
            except Exception as error:result['failures'].append(str(error))
            result.update(status='failed' if result['failures'] else 'passed',wall_ms=round((time.monotonic()-started)*1000),
                maximum_concurrent_analyst_lifecycles=protocol.maximum_concurrent_analysts,role_records=protocol.records)
            usage=[r['usage'] for r in protocol.records]
            result['native_usage']={'requests_with_usage':sum(u['requests'] for u in usage),
                'input_tokens_reported':sum(u['input_tokens'] for u in usage),'output_tokens_reported':sum(u['output_tokens'] for u in usage),
                'http_attempt_records':sum(len(r['requests']) for r in protocol.records),
                'usage_complete':all(r['native_usage_complete'] for r in protocol.records),'declared_zero_api_fee_micros':0}
            save(directory/'result.json',result);report['cases'].append(result);save(root/'report.json',report)
            print(json.dumps({k:result[k] for k in ['id','status','failures','wall_ms','native_usage']},ensure_ascii=False),flush=True)
    report.update(completed=True,passed=sum(r['status']=='passed' for r in report['cases']),failed=sum(r['status']=='failed' for r in report['cases']))
    save(root/'report.json',report)
    raise SystemExit(1 if report['failed'] else 0)


if __name__=='__main__':main()
