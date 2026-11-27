"""Project/export an actual native_skills campaign to a running local Collector.

No model calls or automatic receiver startup here. The earlier native campaign
owns its explicit generation/service caps. Export is explicit, one attempt per
signal, and failures/omissions remain in the result. Use an exclusively owned
Collector debug log for receiver evidence, not a mock HTTP endpoint.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import time
from urllib.parse import urlsplit


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('campaign', type=Path)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--collector-log', type=Path, required=True)
    parser.add_argument('--name', default='observability', help='new create-only campaign subdirectory')
    args = parser.parse_args()
    root = args.campaign.resolve()
    if not re.fullmatch(r'[A-Za-z0-9_-]{1,64}',args.name):
        raise ValueError('invalid acceptance output directory name')
    destination = root / args.name
    destination.mkdir(exist_ok=False)
    (destination / 'harness.py').write_bytes(Path(__file__).read_bytes())
    executable = args.taskloom.resolve()
    campaign = json.loads((root / 'report.json').read_text(encoding='utf-8'))
    config = json.loads((root / 'taskloom.json').read_text(encoding='utf-8'))
    endpoint = config.get('observability', {}).get('otlp', {}).get('endpoint', '')
    if urlsplit(endpoint).hostname not in {'127.0.0.1', '::1', 'localhost'} or not config['observability'].get('namespace'):
        raise ValueError('acceptance requires an explicit local receiver and stable namespace')
    environment = dict(os.environ)
    for name in [config['provider'].get('key_env'), 'TASKLOOM_EXAMPLE_CONNECTOR_TOKEN']:
        if name:
            environment.pop(name, None)

    def cli(configuration, *arguments):
        result = subprocess.run([str(executable), '--config', str(configuration), *arguments], capture_output=True, timeout=60, env=environment)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors='replace'))
        return json.loads(result.stdout)

    report = {'kind': 'actual native journal + actual official Collector acceptance', 'checks': {}, 'exports': [],
              'native_campaign_status': campaign['status'], 'failures': [],
              'taskloom_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'model_and_connector_credential_environments_removed_from_cli': True,
              'no_new_generation_or_embedding_calls': True}
    log_offset=args.collector_log.stat().st_size
    report['collector_log_start_offset']=log_offset
    try:
        # The before-approval snapshot was observed by the actual native runner.
        # Copy it to a separate inspection-only root, preserving original bytes
        # in the campaign report. Never rewind the live workflow/run stores.
        before = campaign['cases']['note_before_approval']
        stage = destination / 'before-approval'
        for directory in ['runs', 'workflows']:
            (stage / directory).mkdir(parents=True, exist_ok=False)
        for directory, identity, value in [('runs', before['child']['report']['run_id'], before['child']),
                                            ('workflows', before['state']['id'], before['state'])]:
            extension='.workflow.json' if directory=='workflows' else '.json'
            save(stage / directory / (hashlib.sha256(identity.encode()).hexdigest() + extension), value)
        staged_config = dict(config, state_dir=str(stage))
        staged_path = destination / 'before.json'
        save(staged_path, staged_config)
        snapshots = [('before', staged_path, 'skill-note'), ('after', root / 'taskloom.json', 'skill-note'),
                     ('research', root / 'taskloom.json', 'skill-research')]
        views = {}
        for name, path, identity in snapshots:
            view = cli(path, 'diagnostics', 'workflow', identity)
            views[name] = view
            save(destination / (name + '-snapshot.json'), view)
            s = view['snapshot']
            receipt = cli(path, 'trace-export', 'workflow', identity, s['source_revision_key'], view['export_binding'])
            save(destination / (name + '-export.json'), receipt)
            report['exports'].append({'name': name, 'receipt': receipt, 'trace_id': s['trace_id']})
        a, b, research = [views[n]['snapshot'] for n in ['before', 'after', 'research']]
        report['checks']['trace_identity_survives_native_approval_restart_and_reconciliation'] = a['trace_id'] == b['trace_id'] and a['root_span_id'] == b['root_span_id']
        old = {s['span_id']: s.get('parent_span_id') for s in a['spans']}
        new = {s['span_id']: s.get('parent_span_id') for s in b['spans']}
        report['checks']['existing_span_identity_and_parentage_retained'] = all(new.get(k) == v and k in new for k, v in old.items())
        report['checks']['progress_changes_source_revision_key'] = a['source_revision_key'] != b['source_revision_key']
        report['checks']['workflow_child_generation_not_double_counted'] = b['metrics']['model_decisions_started'] == campaign['cases']['note_reconciled']['child']['report']['steps']
        report['checks']['uncertain_tool_outcome_is_retained'] = any(s['kind'] == 'tool' and s['outcome'] == 'uncertain' for s in b['spans'])
        report['checks']['actual_approval_wait_and_reconciliation_are_visible'] = any(s['kind']=='approval' and not s['ended'] for s in a['spans']) and any(s['kind']=='approval' and s['ended'] for s in b['spans']) and any(s['kind']=='recovery' for s in b['spans'])
        report['checks']['actual_retrieval_span_with_no_exported_quote'] = any(s['kind'] == 'retrieval' for s in research['spans']) and research['metrics']['retrievals'] >= 1
        report['checks']['service_costs_are_unknown_not_zero'] = all(s['metrics']['embedding_attributed_cost_micros'] is None and s['metrics']['tool_services_attributed_cost_micros'] is None for s in [a, b, research])
        report['checks']['receiver_accepted_both_signals_every_time'] = all(e['receipt']['traces']['status'] == 'accepted' and e['receipt']['metrics']['status'] == 'accepted' for e in report['exports'])
        report['checks']['completed_workflows_have_valid_ended_clocks'] = all(e['receipt']['omitted_open'] == 0 and e['receipt']['omitted_without_clock'] == 0 and e['receipt']['omitted_invalid_clock'] == 0 for e in report['exports'] if e['name'] != 'before')
        # The collector file may flush shortly after its HTTP acknowledgement.
        expected_ids={e['trace_id'] for e in report['exports']}
        for _ in range(50):
            text=args.collector_log.read_bytes()[log_offset:].decode('utf-8',errors='replace')
            if all(identity in text for identity in expected_ids) and 'taskloom.generation.requests' in text:
                break
            time.sleep(0.1)
        (destination / 'collector.log').write_text(text, encoding='utf-8')
        report['checks']['actual_collector_log_contains_trace_ids_and_metrics'] = all(identity in text for identity in expected_ids) and 'taskloom.generation.requests' in text
        private = ['Explain exact approval binding. Search the approval contract and cite its exact source quotation.',
                   'Approval is bound to the exact tool name and arguments. Changing the payload requires a new decision.',
                   'Taskloom skill recovery evidence 2026-11-28.', 'TASKLOOM_LOCAL_API_KEY', 'TASKLOOM_EXAMPLE_CONNECTOR_TOKEN']
        report['checks']['collector_log_excludes_prompt_quote_note_and_credential_names'] = all(value not in text for value in private)
    except Exception as error:
        report['failures'].append(str(error))
    report['failures'].extend('check failed: ' + name for name, passed in report['checks'].items() if not passed)
    report['status'] = 'failed' if report['failures'] else 'passed'
    save(destination / 'report.json', report)
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(1 if report['failures'] else 0)


if __name__ == '__main__':
    main()
