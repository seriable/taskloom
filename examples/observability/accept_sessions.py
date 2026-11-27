"""Verify native managed-session trace continuity through an owned host restart.

Only explicitly zero-priced loopback generation is accepted. Two visible turns
test correlation, not answer quality. A running official Collector is required;
this helper never starts a provider, retries an uncertain effect or edits state.
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

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'benchmarks'))
from native_retrieval import host, save
from native_memory import idle
from accept_native import linked_checks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--collector-log', type=Path, required=True)
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(config['provider']['base_url']).hostname not in {'localhost', '127.0.0.1', '::1'}
            or not config.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in config['prices'])
            or not config.get('observability', {}).get('namespace')
            or urlsplit(config['observability']['otlp']['endpoint']).hostname not in {'localhost', '127.0.0.1', '::1'}):
        raise ValueError('explicit zero-fee loopback model, stable namespace and local receiver required')
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    executable = args.taskloom.resolve()
    subprocess.run([str(executable), 'init', str(root / 'defaults.json')], capture_output=True, check=True)
    defaults = json.loads((root / 'defaults.json').read_text())
    config.update(state_dir=str(root / 'state'), knowledge=False, roles=[], skills=[], mcp=[], memory=None,
                  research={}, retrieval={'mode': 'lexical'}, sandbox=None, sandbox_top_level=False, final_output=None,
                  system='Answer each request in one short sentence. No tools are available.',
                  limits=dict(defaults['limits'], max_steps=4, max_cost_micros=0))
    path = root / 'taskloom.json'
    save(path, config)
    for source in [Path(__file__), Path(__file__).with_name('accept_native.py'),
                   Path(__file__).resolve().parents[1] / 'benchmarks' / 'native_retrieval.py',
                   Path(__file__).resolve().parents[1] / 'benchmarks' / 'native_memory.py']:
        shutil.copy2(source, root / source.name)
    environment = dict(os.environ)
    environment.pop(config['provider']['key_env'], None)
    report = {'version': 1, 'checks': {}, 'failures': [], 'exports': [], 'turns': [],
              'evidence_kind': 'actual zero-fee local native API + process restart + official Collector',
              'fees': {'currency': 'USD', 'input_per_million': 0, 'output_per_million': 0, 'total_cap_micros': 0,
                       'credential_environment': config['provider']['key_env'], 'excluded': 'hardware/electricity/current-chat'},
              'framework_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'limitations': ['visible two-turn integration, not independent answer-quality or throughput evidence',
                              'host restart occurs between completed turns, not an in-flight crash',
                              'overlapping selected-scope gauges must not be summed']}
    log_offset = args.collector_log.stat().st_size
    report['collector_log_start_offset'] = log_offset
    started = time.perf_counter()

    def cli(kind, identity):
        result = subprocess.run([str(executable), '--config', str(path), 'diagnostics', kind, identity],
                                capture_output=True, timeout=60, env=environment)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors='replace'))
        return json.loads(result.stdout)

    def inspect_and_export(api, label, kind, identity):
        view = api('/diagnostics/' + kind + '/' + identity)
        cold = cli(kind, identity)
        # capture time is observational; all persisted data and export bindings must agree.
        def frozen(value):
            value = json.loads(json.dumps(value))
            value['snapshot'].pop('captured_unix_ms', None)
            return value
        report['checks'][label + '_cold_cli_matches_api_without_provider_key'] = frozen(cold) == frozen(view)
        save(root / (label + '-snapshot.json'), view)
        snapshot = view['snapshot']
        receipt = api('/diagnostics/' + kind + '/' + identity + '/export',
                      {'source_revision_key': snapshot['source_revision_key'], 'export_binding': view['export_binding']})
        save(root / (label + '-export.json'), receipt)
        report['exports'].append({'name': label, 'trace_id': snapshot['trace_id'], 'receipt': receipt})
        return snapshot

    try:
        with host(executable, path, root) as api:
            api('/sessions', {'id': 'trace-session'})
            acknowledged = api('/sessions/trace-session/turns', {'turn_id': 'first', 'prompt': 'What is two plus two?'})
            save(root / 'first-acknowledged.json', acknowledged)
            initial = api('/runs/' + acknowledged['run_id'])['state']
            save(root / 'first-acknowledged-child.json', initial)
            report['checks']['acknowledged_child_already_has_host_binding'] = initial.get('trace_binding') == {
                'type': 'session', 'session_id': 'trace-session', 'turn_id': 'first'}
            session = idle(api, 'trace-session')
            run_id = session['turns'][0]['binding']['run_id']
            run = api('/runs/' + run_id)['state']
            save(root / 'first-state.json', {'session': session, 'run': run})
            first = inspect_and_export(api, 'first-parent', 'session', 'trace-session')
            child = inspect_and_export(api, 'first-child', 'run', run_id)
            for check, passed in linked_checks(first, child).items():
                report['checks']['first_' + check] = passed
            report['turns'].append(run['report'])
        with host(executable, path, root) as api:
            restored = inspect_and_export(api, 'restarted-parent', 'session', 'trace-session')
            report['checks']['same_journal_trace_after_process_restart'] = (
                restored['trace_id'] == first['trace_id'] and restored['source_revision_key'] == first['source_revision_key']
                and restored['spans'] == first['spans'])
            try:
                api('/sessions/trace-session/resume', {})
                report['checks']['closed_session_resume_refused'] = False
            except RuntimeError as error:
                report['checks']['closed_session_resume_refused'] = str(error).startswith('HTTP 409:') and 'no active turn' in str(error)
            replay = api('/runs/' + run_id)['state']
            report['checks']['terminal_resume_does_not_change_run_or_repeat_model'] = run == replay
            api('/sessions/trace-session/turns', {'turn_id': 'second', 'prompt': 'Repeat your previous answer briefly.'})
            session = idle(api, 'trace-session')
            second_id = session['turns'][1]['binding']['run_id']
            second_run = api('/runs/' + second_id)['state']
            save(root / 'second-state.json', {'session': session, 'run': second_run})
            second = inspect_and_export(api, 'second-parent', 'session', 'trace-session')
            second_child = inspect_and_export(api, 'second-child', 'run', second_id)
            first_again = inspect_and_export(api, 'first-child-after-restart', 'run', run_id)
            for check, passed in linked_checks(second, second_child).items():
                report['checks']['second_' + check] = passed
            report['checks']['prior_child_observations_unchanged_after_next_turn'] = child['spans'] == first_again['spans'] and child['source_revision_key'] == first_again['source_revision_key']
            report['checks']['new_turn_retains_trace_and_distinct_metric_scope'] = second['trace_id'] == first['trace_id'] and child['selected_span_id'] != second_child['selected_span_id']
            report['checks']['sibling_revision_keys_do_not_collide'] = child['source_revision_key'] != second_child['source_revision_key']
            report['checks']['session_generation_counted_once'] = second['metrics']['model_decisions_started'] == run['report']['steps'] + second_run['report']['steps']
            report['turns'].append(second_run['report'])
            report['checks']['two_native_turns_completed'] = all(t['stop_reason'] == 'completed' for t in report['turns'])
        report['checks']['all_exports_accepted_both_signals'] = all(e['receipt']['traces']['status'] == 'accepted' and e['receipt']['metrics']['status'] == 'accepted' for e in report['exports'])
        report['checks']['context_and_open_parent_omissions_are_distinct'] = all(
            e['receipt']['omitted_context'] == (2 if 'child' in e['name'] else 0)
            and e['receipt']['omitted_open'] == (0 if 'child' in e['name'] else 1)
            and e['receipt']['omitted_without_clock'] == 0 and e['receipt']['omitted_invalid_clock'] == 0
            for e in report['exports'])
        for _ in range(50):
            text = args.collector_log.read_bytes()[log_offset:].decode('utf-8', errors='replace')
            if 'taskloom.selected_span_id' in text and all(e['receipt']['selected_span_id'] in text for e in report['exports']):
                break
            time.sleep(0.1)
        (root / 'collector.log').write_text(text, encoding='utf-8')
        report['checks']['receiver_log_contains_trace_and_all_selected_metric_scopes'] = 'taskloom.selected_span_id' in text and all(
            e['trace_id'] in text and e['receipt']['selected_span_id'] in text for e in report['exports'])
        report['checks']['receiver_does_not_contain_prompts_answers_or_credential_name'] = all(
            s not in text for s in ['What is two plus two?', 'Repeat your previous answer briefly.', config['provider']['key_env']]) and all(
            t['messages'][-1]['turn']['content'] not in text for t in report['turns'])
    except Exception as error:
        report['failures'].append(str(error))
    report['failures'].extend('check failed: ' + name for name, passed in report['checks'].items() if not passed)
    report['status'] = 'failed' if report['failures'] else 'passed'
    report['wall_ms'] = round((time.perf_counter() - started) * 1000)
    save(root / 'report.json', report)
    print(json.dumps({k: v for k, v in report.items() if k != 'turns'}, ensure_ascii=False))
    raise SystemExit(1 if report['failures'] else 0)


if __name__ == '__main__':
    main()
