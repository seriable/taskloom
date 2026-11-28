"""Inspect actual persisted campaign results after HTTP observation failure.

No model substitution or new native requests: the old loopback model transport
must be stopped. A separate real host observes/resumes finished workflows only;
any changed runtime file or child is a failure. Mechanical checks remain
separate from semantic truth, and original campaign failures stay unchanged.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.parse import urlsplit

from native_retrieval import host, save


def runtime_hashes(root):
    return {str(path.relative_to(root)):hashlib.sha256(path.read_bytes()).hexdigest()
            for folder in ['runs','workflows'] for path in sorted((root/'state'/folder).glob('*.json'))}


def audit_output(case, state, children, verifier):
    checks={'workflow_completed':state['status']=='completed'}
    work_ids={n['id'] for n in state['definition']['nodes'] if n['task']=='analyst'}
    active=set();maximum=0
    for event in state['events']:
        kind=event['kind']
        if kind.get('node') in work_ids:
            if kind['type']=='node_started':active.add(kind['node'])
            else:active.discard(kind['node'])
            maximum=max(maximum,len(active))
    checks['required_role_overlap']=not case['require_parallel'] or maximum>=2
    completed=sum(n['id'] in work_ids and n['status']=='complete' for n in state['nodes'])
    checks['required_analyst_completion']=completed>=case.get('min_completed_analysts',0)
    usage={key:sum(c['report']['usage'][key] for c in children.values())
           for key in ['requests','input_tokens','output_tokens','cost_micros']}
    checks['required_native_requests']=usage['requests']>=case.get('min_native_requests',0)
    rework=any(n['id']=='draft-1' and n['status']=='complete' for n in state['nodes'])
    initial=next((n.get('output') for n in state['nodes'] if n['id']=='review-initial'),None)
    if case.get('initial_draft'):
        rework=(isinstance(initial,dict) and initial.get('approved') is False and bool(initial.get('feedback','').strip())
                and any(n['id']=='draft' and n['status']=='complete' for n in state['nodes']))
    checks['required_rejection_rewrite']=not case['require_rework'] or rework
    final_id='result' if case['kind']=='peer' else 'draft'
    final=next((n.get('output') for n in state['nodes'] if n['id']==final_id),None)
    if case['kind']=='peer' and isinstance(final,dict):final=final.get('answer')
    if isinstance(final,str):final=json.loads(final)
    if not isinstance(final,dict):raise ValueError('no actual final report')
    checks['required_topic_words']=all(p.lower() in final['text'].lower() for p in case['required_phrases'])
    reviews=[n['output'] for n in state['nodes'] if n['id'] in ['review','review-0','review-1'] and n['status']=='complete']
    checks['native_final_review_approved']=bool(reviews) and reviews[-1].get('approved') is True
    evidence=[]
    for child in children.values():
        for step in child['tool_steps']:
            progress=step['progress']
            if step['call']['name']=='knowledge_search' and progress['type']=='complete' and not progress['result']['is_error']:
                evidence.append(progress['result']['output'])
    result=subprocess.run([str(verifier)],input=json.dumps({'report':final,'evidence':evidence}).encode(),capture_output=True,timeout=10)
    checks['exact_citation_verification']=result.returncode==0
    verified=json.loads(result.stdout) if result.returncode==0 else None
    checks['required_source_coverage']=verified is not None and set(case['required_documents'])<={s['document_id'] for s in verified['sources']}
    return {'checks':checks,'usage':usage,'maximum_concurrent_analysts':maximum,'final_report':final,
            'verified':verified,'initial_review':initial,'final_reviews':reviews,
            'semantic_support':'Unverified here; exact sources, topic words and model review are not independent factual assessment.'}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('campaign',type=Path)
    parser.add_argument('out',type=Path)
    parser.add_argument('--taskloom',type=Path,required=True)
    parser.add_argument('--verifier',type=Path,required=True)
    args=parser.parse_args()
    root=args.campaign.resolve();out=args.out.resolve()
    config=json.loads((root/'taskloom.json').read_text(encoding='utf-8'))
    url=urlsplit(config['provider']['base_url'])
    if (url.scheme!='http' or url.hostname not in {'127.0.0.1','localhost','::1'}
            or Path(config['state_dir']).resolve()!=root/'state'
            or any(config.get(k) for k in ['mcp','skills','database','memory','sandbox','sandbox_top_level','observability'])
            or any(config.get('research',{}).values())):
        raise ValueError('isolated cold native campaign and loopback transport required')
    # Require the old model transport to be unreachable before providing even
    # a disposable local credential. Never reconnect a remote paid provider.
    import socket
    try:
        with socket.create_connection((url.hostname,url.port),timeout=1):
            raise ValueError('old model transport is still listening; refuse cold no-dispatch audit')
    except OSError:pass
    suite=json.loads((root/'suite.json').read_text(encoding='utf-8'))
    original=json.loads((root/'report.json').read_text(encoding='utf-8'))
    out.mkdir(parents=True,exist_ok=False)
    before_hashes=runtime_hashes(root)
    report={'scope':__doc__,'original_campaign_report_sha256':hashlib.sha256((root/'report.json').read_bytes()).hexdigest(),
            'original_passed':original['passed'],'original_failed':original['failed'],'cases':[],
            'model_transport_stopped':True,'model_calls_requested_by_audit':0,'completed':False}
    key_env=config['provider']['key_env'];previous=os.environ.get(key_env)
    os.environ[key_env]='cold-audit-no-remote-credential'
    try:
        with host(args.taskloom.resolve(),root/'taskloom.json',out) as api:
            for case in suite['cases']:
                observed=api('/workflows/'+case['id']);state=observed['state']
                if state['status']!='completed':raise ValueError('cold workflow is unfinished; no automatic resume or abandonment')
                children={n['id']:api('/runs/'+n['child']['run_id'])['state'] for n in state['nodes'] if n.get('child')}
                record={'id':case['id'],'before':observed,'children':children,
                        'mechanical_output_audit':audit_output(case,state,children,args.verifier.resolve())}
                api('/workflows/'+case['id']+'/resume',{})
                for _ in range(100):
                    replay=api('/workflows/'+case['id'])
                    if not replay['job']['active']:break
                    time.sleep(.1)
                else:raise RuntimeError('cold terminal reentry did not become inactive')
                record['after']=replay
                record['terminal_reentry_unchanged']=replay['state']==state
                record['passed']=record['terminal_reentry_unchanged'] and all(record['mechanical_output_audit']['checks'].values())
                report['cases'].append(record);save(out/'report.json',report)
    finally:
        if previous is None:os.environ.pop(key_env,None)
        else:os.environ[key_env]=previous
        report['runtime_files_unchanged']=runtime_hashes(root)==before_hashes
        save(out/'report.json',report)
    report.update(completed=True,passed=sum(c['passed'] for c in report['cases']))
    report['all_passed']=report['runtime_files_unchanged'] and report['passed']==len(suite['cases'])
    save(out/'report.json',report)
    print(json.dumps({'passed':report['passed'],'requested':len(suite['cases']),'runtime_files_unchanged':report['runtime_files_unchanged']}))
    raise SystemExit(0 if report['all_passed'] else 1)


if __name__=='__main__':main()
