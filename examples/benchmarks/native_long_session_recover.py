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
from native_long_session import wait_turn, answer_length, grounded_assertions, required_sources_present
from native_subscription import metadata_for


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--taskloom',type=Path,required=True)
    parser.add_argument('--recover-from',type=Path,required=True)
    parser.add_argument('--context-bytes',type=int,default=65536)
    parser.add_argument('--previous-recovery',type=Path,help='explicit known failed read-only recovery to acknowledge; no automatic retry')
    parser.add_argument('--pin-first-user',action='store_true',help='explicit new-turn context policy keeps the actual first User message, without injecting its reference')
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
    required_documents={pin['document_id'] for pin in original['source_pins']}
    if len(required_documents)!=6:
        raise ValueError('the original recovery task must bind six distinct source documents')
    prior_recovery=None
    if args.previous_recovery:
        parent=args.previous_recovery.resolve()
        prior_recovery=json.loads((parent/'report.json').read_text(encoding='utf-8'))
        if (not prior_recovery.get('completed') or prior_recovery.get('status')!='failed'
                or (prior_recovery['run']['state']['report']['stop_reason']!='deadline'
                    and not (prior_recovery['run']['state']['report']['stop_reason']=='completed'
                        and bool(prior_recovery.get('failures'))
                        and set(prior_recovery['failures'])<={'original_reference_retained','all_six_actual_sources','word_limit'}))
                or Path(prior_recovery['configuration']['state_dir']).resolve()!=previous/'state'):
            raise ValueError('previous recovery must be the same session known deadline or closed answer with explicit reference/source/length quality failure')
        cfg=json.loads((parent/'app.json').read_text(encoding='utf-8'))
        old_state=prior_recovery['run']['state']
        if any(t['progress']['type']!='complete' for t in old_state['tool_steps']):
            raise ValueError('uncertain tool observations cannot be acknowledged by this read-only example')
    if any(t['definition']['effect']!='read_only' for t in old_state['tool_steps']):
        raise ValueError('this bounded recovery example accepts read-only source tasks only')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    failed_id=old_state['report']['run_id']
    old_files={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in (previous/'state/runs').glob('*.json')}
    prior_context=json.loads(json.dumps(cfg['context']))
    cfg.update(provider=provider['provider'],prices=provider['prices'])
    cfg['enable_thinking']=provider.get('enable_thinking')
    cfg['context']['max_input_bytes']=args.context_bytes
    cfg['context']['pin_first_user']=args.pin_first_user
    cfg['final_output']['required_documents']=sorted(required_documents)
    cfg['final_output']['min_sources']=len(required_documents)
    save(root/'app.json',cfg)
    report={'scope':__doc__,'original_failed_campaign_sha256':hashlib.sha256((previous/'report.json').read_bytes()).hexdigest(),
            'original_failed_status':original.get('status'),'configuration':cfg,'subscription':metadata_for(provider),
            'original_failed_context_bytes':original['configuration']['context']['max_input_bytes'],
            'old_context_bytes':prior_context['max_input_bytes'],'new_context_bytes':args.context_bytes,'prior_context_policy':prior_context,
            'checks':{},'completed':False,'failures':[],'logical_stages':12,'attempts_including_recovery':None,
            'previous_recovery':str(args.previous_recovery) if prior_recovery else None,
            'previous_recovery_report_sha256':hashlib.sha256((parent/'report.json').read_bytes()).hexdigest() if prior_recovery else None,
            'previous_run_file_sha256':old_files,
            'previous_run_file_scope':'All pre-existing run checkpoint and .budget.json files; lock files excluded. No unknown generation reservation is released.',
            'required_document_ids':sorted(required_documents),
            'new_turn_source_contract':'All original six documents required by the host output policy before dispatch; prior runs retain their original weaker contracts.',
            'pinning_actual_first_user':args.pin_first_user,
            'reasoning_conditions_changed_for_new_turn_only':cfg.get('enable_thinking')!=original['configuration'].get('enable_thinking')}
    save(root/'report.json',report)
    with host(args.taskloom.resolve(),root/'app.json',root) as api:
        before=api('/sessions/repository-review')['session'];report['before_session']=before
        report['attempts_including_recovery']=len(before['turns'])+int(before['active'] is not None)+1
        already_closed=old_state['report']['stop_reason']=='completed'
        report['checks']['same_occupied_failed_final_turn']=(before['active'] is None and before['turns'][-1]['binding']['run_id']==failed_id) if already_closed else before['active']['run_id']==failed_id
        report['old_failed_run_before']=api('/runs/'+failed_id)['state']
        if not report['checks']['same_occupied_failed_final_turn']:raise ValueError('session no longer matches failed turn')
        acknowledged=before if already_closed else api('/sessions/repository-review/abandon',{'actor':'local-operator',
            'note':'Explicitly acknowledge the known failed read-only turn. Preserve its checkpoint, known observations and uncertain generation reservations. A distinct new turn uses explicit context/reasoning configuration; no unknown tool effect is retried.'})['session']
        report['acknowledged_session']=acknowledged
        report['checks']['failed_turn_explicitly_acknowledged']=acknowledged['active'] is None and len(acknowledged['turns'])==len(before['turns'])+(0 if already_closed else 1)
        report['acknowledgement_kind']='previous runtime turn already closed; acknowledge only its separately recorded quality failure, no abandon API invoked' if already_closed else 'explicit abandon of known failed terminal runtime turn'
        request={'turn_id':'stage-12-recovery-'+hashlib.sha256(failed_id.encode()).hexdigest()[:12],
                 'prompt':failed['prompt']+'\nFormatting clarification for this explicit recovery turn: keep source hashes only in the sources array, never copy hashes into text. Preserve every requested topic within the existing text word bound and exact Findings/task-context sections. This is a new turn; do not claim that an earlier failed turn passed.'}
        report['new_turn_request']=request;save(root/'report.json',report)
        launched=api('/sessions/repository-review/turns',request)
        state,conversation=wait_turn(api,'repository-review',launched['run_id'])
        report.update(run=state,session=conversation['session'])
        if args.pin_first_user:
            first=next(i for i,m in enumerate(state['state']['report']['messages']) if m['role']=='user')
            report['checks']['actual_first_user_is_retained']=all(first in c['retained_indices'] for c in state['state']['report']['contexts'])
        report['checks']['new_distinct_run']=launched['run_id']!=failed_id
        report['checks']['new_turn_completed']=state['state']['report']['stop_reason']=='completed' and conversation['session']['active'] is None
        if report['checks']['new_turn_completed']:
            verified=api('/runs/'+launched['run_id']+'/report');report['verified']=verified
            report['source_assertions']=grounded_assertions(verified)
            reference=original['turns'][0]['source_assertions']['reference']
            report['checks']['original_reference_retained']=report['source_assertions']['reference']==reference
            report['checks']['all_six_actual_sources']=required_sources_present(verified,required_documents)
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
