"""Explicitly close a known ContextLimit turn and continue the SAME session.

The failed run is never changed or resumed under new bindings. An authenticated
operator acknowledges its known terminal outcome, then a new turn uses the same
user request with a larger declared context. Fresh retrieval is still required.
No missing reference, tool result or model answer is inserted by the harness.
"""
import argparse
import hashlib
import json
from pathlib import Path
import time

from native_retrieval import host, save
from native_long_session import wait_turn, answer_length, grounded_assertions
from native_subscription import metadata_for


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--taskloom',type=Path,required=True)
    parser.add_argument('--recover-from',type=Path,required=True)
    parser.add_argument('--context-bytes',type=int,default=65536)
    args=parser.parse_args()
    previous=args.recover_from.resolve()
    original=json.loads((previous/'report.json').read_text(encoding='utf-8'))
    cfg=json.loads((previous/'app.json').read_text(encoding='utf-8'))
    provider=json.loads(args.config.read_text(encoding='utf-8'))
    failed=original['turns'][-1]
    old_state=failed['run']['state']
    if (len(original['turns'])!=12 or old_state['report']['stop_reason']!='context_limit'
            or not original.get('grounded_sections') or cfg['model']!=provider['model']
            or not cfg['context']['max_input_bytes']<args.context_bytes<=262144
            or any(t['progress']['type']!='complete' for t in old_state['tool_steps'])):
        raise ValueError('requires a known ContextLimit final turn, completed read observations and explicit larger context')
    if any(t['definition']['effect']!='read_only' for t in old_state['tool_steps']):
        raise ValueError('this bounded recovery example accepts read-only source tasks only')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    failed_id=old_state['report']['run_id']
    old_files={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in (previous/'state/runs').glob('*.json')}
    cfg.update(provider=provider['provider'],prices=provider['prices'])
    cfg['context']['max_input_bytes']=args.context_bytes
    save(root/'app.json',cfg)
    report={'scope':__doc__,'original_failed_campaign_sha256':hashlib.sha256((previous/'report.json').read_bytes()).hexdigest(),
            'original_failed_status':original.get('status'),'configuration':cfg,'subscription':metadata_for(provider),
            'old_context_bytes':original['configuration']['context']['max_input_bytes'],'new_context_bytes':args.context_bytes,
            'checks':{},'completed':False,'failures':[],'logical_stages':12,'attempts_including_recovery':13}
    save(root/'report.json',report)
    with host(args.taskloom.resolve(),root/'app.json',root) as api:
        before=api('/sessions/repository-review')['session'];report['before_session']=before
        report['checks']['same_occupied_failed_final_turn']=before['active']['run_id']==failed_id
        report['old_failed_run_before']=api('/runs/'+failed_id)['state']
        if not report['checks']['same_occupied_failed_final_turn']:raise ValueError('session no longer matches failed turn')
        acknowledged=api('/sessions/repository-review/abandon',{'actor':'local-operator',
            'note':'Explicitly acknowledge ContextLimit after completed read-only retrieval. Preserve the failed run and known observations. A distinct new turn will use a larger host context; no unknown effect is retried.'})['session']
        report['acknowledged_session']=acknowledged
        report['checks']['failed_turn_explicitly_acknowledged']=acknowledged['active'] is None and len(acknowledged['turns'])==12
        request={'turn_id':'stage-12-context-recovery','prompt':failed['prompt']}
        report['new_turn_request']=request;save(root/'report.json',report)
        launched=api('/sessions/repository-review/turns',request)
        state,conversation=wait_turn(api,'repository-review',launched['run_id'])
        report.update(run=state,session=conversation['session'])
        report['checks']['new_distinct_run']=launched['run_id']!=failed_id
        report['checks']['new_turn_completed']=state['state']['report']['stop_reason']=='completed' and conversation['session']['active'] is None
        if report['checks']['new_turn_completed']:
            verified=api('/runs/'+launched['run_id']+'/report');report['verified']=verified
            report['source_assertions']=grounded_assertions(verified)
            reference=original['turns'][0]['source_assertions']['reference']
            report['checks']['original_reference_retained']=report['source_assertions']['reference']==reference
            report['checks']['all_six_actual_sources']=len({s['document_id'] for s in verified['sources']})==6
            report['checks']['word_limit']=answer_length(verified,12)['passed']
            report['checks']['bounded_compression_exercised']=any(c['summary'] is not None for c in state['state']['report']['contexts'])
            before_replay=conversation['session']
            api('/sessions/repository-review/turns',request)
            replay,after_session=wait_turn(api,'repository-review',launched['run_id'])
            report['checks']['terminal_reentry_unchanged']=replay['state']==state['state'] and after_session['session']==before_replay
        report['old_failed_run_after']=api('/runs/'+failed_id)['state']
        report['checks']['old_failed_checkpoint_unchanged']=report['old_failed_run_after']==report['old_failed_run_before']
    report['checks']['all_previous_run_files_unchanged']=all(Path(path).exists() and hashlib.sha256(Path(path).read_bytes()).hexdigest()==digest for path,digest in old_files.items())
    report['usage']=report['run']['state']['report']['usage']
    report['failures']=[key for key,value in report['checks'].items() if not value]
    report.update(completed=True,status='failed' if report['failures'] else 'passed')
    save(root/'report.json',report)
    print(json.dumps({'status':report['status'],'checks':report['checks'],'usage':report['usage']}))
    raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':main()
