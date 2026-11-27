"""Install source skills and exercise native roles plus real MCP connectors.

The model API must be explicitly zero-priced and loopback. The example MCP
service reads public GitHub metadata and writes only an owned note directory.
Approval/reconciliation are explicit harness-operator decisions, not model
autonomy or an independent quality benchmark. All failures are retained.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request
from urllib.parse import urlsplit
import uuid
import hashlib

from native_retrieval import host, save


def workflow(api, identity):
    for _ in range(1800):
        observed = api('/workflows/' + identity)
        if not observed['job']['active']:
            if observed['job']['error']:
                raise RuntimeError(observed['job']['error'])
            return observed['state']
        time.sleep(0.1)
    raise RuntimeError('workflow exceeded harness deadline')


def child(api, state):
    node = next(n for n in state['nodes'] if n['id'] == 'result')
    return api('/runs/' + node['child']['run_id'])['state'] if node.get('child') else None


def start(api, identity, mount, template, data):
    api('/templates/' + mount + '/' + template + '/runs', {'id': identity, 'input': data,
        'limits': {'max_cost_micros': 0, 'max_nodes': 32, 'max_parallel': 2, 'max_dispatches': 64,
                   'max_output_bytes': 131072, 'max_duration': {'secs': 300, 'nanos': 0},
                   'shutdown_grace': {'secs': 5, 'nanos': 0}}})
    return workflow(api, identity)


def cli(executable, config, *args, environment=None):
    result = subprocess.run([str(executable), '--config', str(config), *map(str, args)], capture_output=True, timeout=60, env=environment)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace'))
    return json.loads(result.stdout)


def operator_status(endpoint, token):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *unused):
            return None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    def request(body, session=None):
        headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                   'Accept': 'application/json, text/event-stream'}
        if session:
            headers.update({'Mcp-Session-Id': session, 'MCP-Protocol-Version': '2025-06-18'})
        with opener.open(urllib.request.Request(endpoint, data=json.dumps(body).encode(), headers=headers), timeout=10) as response:
            raw = response.read(65536)
            return (json.loads(raw) if raw else None), response.headers.get('Mcp-Session-Id')
    _, session = request({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'explicit-harness-operator', 'version': '1'}}})
    request({'jsonrpc': '2.0', 'method': 'notifications/initialized'}, session)
    result, _ = request({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'notes_status', 'arguments': {}}}, session)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--repository', default='seriable/taskloom')
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(config['provider']['base_url']).hostname not in {'localhost', '127.0.0.1', '::1'}
            or not config.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in config['prices'])):
        raise ValueError('this example only supports explicitly zero-fee local model APIs')
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    executable = args.taskloom.resolve()
    source = Path(__file__).resolve().parents[1] / 'skills'
    shutil.copy2(__file__, root / 'harness.py')
    shutil.copy2(Path(__file__).with_name('native_retrieval.py'), root / 'native_retrieval.py')
    shutil.copytree(source, root / 'skill-sources', ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
    spec = importlib.util.spec_from_file_location('skill_pack', source / 'pack.py')
    pack = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pack)
    subprocess.run([str(executable), 'init', str(root / 'defaults.json')], capture_output=True, check=True)
    defaults = json.loads((root / 'defaults.json').read_text())
    config.update(state_dir=str(root / 'state'), knowledge=True, roles=[], skills=[], mcp=[], research={},
                  retrieval={'mode': 'lexical'}, sandbox=None, sandbox_top_level=False, memory=None, final_output=None,
                  limits=dict(defaults['limits'], max_cost_micros=0))
    config_path = root / 'taskloom.json'
    save(config_path, config)
    report = {'version': 1, 'completed': False, 'cases': {}, 'checks': {}, 'failures': [],
              'framework_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'evidence_kind': 'actual local native model + source packages + actual MCP/filesystem/public GitHub',
              'fees': {'currency': 'USD', 'model_input_per_million': 0, 'model_output_per_million': 0,
                       'generation_cap_micros': 0, 'model_credential_environment': config['provider']['key_env'],
                       'connector_credential_environment': 'TASKLOOM_EXAMPLE_CONNECTOR_TOKEN',
                       'public_github_api_price_per_call': 0, 'github_request_cap': 20,
                       'excluded': 'hardware/electricity and current-chat usage'},
              'limitations': ['visible tasks, no competitor or blind quality comparison',
                              'approval and observed-result reconciliation are explicit operator actions',
                              'source package installation never starts the connector service',
                              'recovery-only still requires original exact host tool contracts and grants',
                              'retirement takes effect on new host activation; an already loaded process keeps its frozen snapshot']}
    service = log = None
    started = time.perf_counter()
    try:
        pins = {}
        install_environment = dict(os.environ)
        install_environment.pop(config['provider']['key_env'], None)
        install_environment.pop('TASKLOOM_EXAMPLE_CONNECTOR_TOKEN', None)
        for revision, name in enumerate(['evidence-base', 'research-brief', 'connector-notes']):
            archive = root / (name + '.tar')
            pack.pack(source / name, archive)
            installed = cli(executable, config_path, 'skill-install', archive, revision, environment=install_environment)
            pins[name] = installed['package']
            if installed['activated'] is not False:
                raise RuntimeError('installation unexpectedly activated a package')
        report['installed'] = pins
        report['checks']['install_inert_without_services'] = True
        token = uuid.uuid4().hex
        os.environ['TASKLOOM_EXAMPLE_CONNECTOR_TOKEN'] = token
        ready = root / 'connector-ready.json'
        log = (root / 'connector.log').open('wb')
        service = subprocess.Popen([sys.executable, '-u', str(source / 'connector_service.py'), '--notes', str(root / 'notes'),
            '--repository', args.repository, '--port', '0', '--ready-file', str(ready), '--fail-after-append-once'],
            stdout=log, stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        for _ in range(150):
            if ready.exists():
                break
            if service.poll() is not None:
                raise RuntimeError('example MCP service exited; inspect connector.log')
            time.sleep(0.1)
        else:
            raise RuntimeError('example MCP service readiness timed out')
        endpoint = json.loads(ready.read_text())['endpoint']
        config['mcp'] = json.loads((source / 'connector-notes' / 'connector.example.json').read_text())['mcp']
        config['mcp'][0]['endpoint'] = endpoint
        config['skills'] = [
            {'package': pins['research-brief'], 'mount': 'research', 'grant': {'tools': ['knowledge_search'], 'permissions': ['knowledge:read'], 'credentials': []}},
            {'package': pins['connector-notes'], 'mount': 'connect', 'grant': {'tools': ['repo_metadata', 'notes_append', 'notes_status'],
                'permissions': ['repository:read', 'notes:write', 'notes:read'], 'credentials': ['TASKLOOM_EXAMPLE_CONNECTOR_TOKEN']}}]
        save(config_path, config)
        wanted = 'Taskloom skill recovery evidence 2026-11-28.'
        with host(executable, config_path, root) as api:
            report['checks']['mounted_templates_available'] = len(api('/templates')['templates']) == 4
            api('/knowledge', {'expected_revision': 0, 'document': {'id': 'approval', 'title': 'Approval contract', 'source': 'fixture:skill-approval-v1',
                'text': 'Approval is bound to the exact tool name and arguments. Changing the payload requires a new decision.', 'metadata': {}}})
            research = start(api, 'skill-research', 'research', 'source-summary', {'goal': 'Explain exact approval binding. Search the approval contract and cite its exact source quotation.'})
            report['cases']['research'] = {'state': research, 'child': child(api, research)}
            if research['status'] != 'completed':
                report['failures'].append('native cited research template did not complete')
            repository = start(api, 'repo-info', 'connect', 'repository-summary', {'goal': 'Use repo_metadata to summarize the repository pinned by the host. Report its actual name and license.'})
            repository_child = child(api, repository)
            report['cases']['repository'] = {'state': repository, 'child': repository_child}
            observations = root / 'notes' / 'observations.json'
            observed = json.loads(observations.read_text()) if observations.exists() else None
            save(root / 'github-observations.json', observed)
            output = next(n.get('output') for n in repository['nodes'] if n['id'] == 'result')
            expected = observed['observations'][-1].get('result') if observed and observed['observations'] else None
            report['checks']['real_github_source_matches_native_output'] = bool(expected and isinstance(output, dict) and output.get('repository') == expected['full_name'] and output.get('license') == expected['license'])
            status_workflow = start(api, 'notes-status', 'connect', 'notes-status', {})
            report['cases']['notes_status'] = {'state': status_workflow, 'child': child(api, status_workflow)}
            report['checks']['native_status_template_reads_actual_empty_store'] = status_workflow['status'] == 'completed' and next(n.get('output') for n in status_workflow['nodes'] if n['id'] == 'result') == {'entries': 0}
            note = start(api, 'skill-note', 'connect', 'record-note', {'text': wanted})
            waiting = child(api, note)
            report['cases']['note_before_approval'] = {'state': note, 'child': waiting}
            pending = next((step['progress']['request'] for step in waiting['tool_steps'] if step['progress']['type'] == 'awaiting_approval'), None) if waiting else None
            if not pending or pending['call']['name'] != 'notes_append' or pending['call']['arguments'] != {'text': wanted}:
                raise RuntimeError('model did not propose the exact requested note; no payload was rewritten or approved')
            report['checks']['no_write_before_exact_approval'] = not (root / 'notes' / 'notes.jsonl').exists()
        save(root / 'connector-pin.json', pins['connector-notes'])
        cli(executable, config_path, 'skill-retire', root / 'connector-pin.json', 3)
        denied = subprocess.run([str(executable), '--config', str(config_path), 'invalid-no-dispatch-command'], capture_output=True, timeout=30)
        report['checks']['retired_active_pin_refuses_host_activation'] = denied.returncode != 0 and 'unavailable for new activation' in denied.stderr.decode(errors='replace')
        config['skills'][1]['mode'] = 'recovery_only'
        recovery_path = root / 'recovery.json'
        save(recovery_path, config)
        with host(executable, recovery_path, root) as api:
            report['checks']['recovery_only_templates_hidden'] = all(t['mount'] != 'connect' for t in api('/templates')['templates'])
            try:
                start(api, 'forbidden-new', 'connect', 'record-note', {'text': 'must not write'})
            except RuntimeError as error:
                report['checks']['recovery_only_cannot_start_new_template'] = 'HTTP 403:' in str(error)
            else:
                report['checks']['recovery_only_cannot_start_new_template'] = False
            api('/workflows/skill-note/nodes/result/approvals/' + pending['request_id'],
                {'approved': True, 'actor': 'explicit-harness-operator', 'note': 'checked exact notes_append name and requested text before dispatch'})
            api('/workflows/skill-note/resume', {})
            uncertain = workflow(api, 'skill-note')
            unknown_child = child(api, uncertain)
            report['cases']['note_response_lost'] = {'state': uncertain, 'child': unknown_child}
            uncertain_step = next((s for s in unknown_child['tool_steps'] if s['progress']['type'] == 'uncertain'), None)
            if not uncertain_step:
                raise RuntimeError('response-loss case did not retain an uncertain tool step')
            status = operator_status(endpoint, token)
            save(root / 'operator-observed-status.json', status)
            observed_note = status['result']['structuredContent']
            report['checks']['one_persisted_note_despite_response_loss'] = observed_note == {'entries': 1, 'latest': [{'text': wanted}]}
            api('/workflows/skill-note/resume', {})
            no_retry = workflow(api, 'skill-note')
            report['checks']['uncertain_resume_does_not_repeat_write'] = operator_status(endpoint, token)['result']['structuredContent']['entries'] == 1 and child(api, no_retry)['report']['steps'] == unknown_child['report']['steps']
            observation = {'saved': True, 'entries': 1, 'confirmed_from': 'explicit operator read of persisted note after response loss'}
            api('/workflows/skill-note/nodes/result/reconcile/' + uncertain_step['call']['id'],
                {'type': 'record_result', 'output': {'content': [{'type': 'text', 'text': json.dumps(observation)}],
                 'structuredContent': observation, 'isError': False}, 'is_error': False, 'actor': 'explicit-harness-operator',
                 'note': 'actual notes_status confirmed one exact persisted note; record observed success without repeating the append'})
            api('/workflows/skill-note/resume', {})
            done = workflow(api, 'skill-note')
            report['cases']['note_reconciled'] = {'state': done, 'child': child(api, done), 'reconciliation_output_is_operator_observation': True}
            report['checks']['native_role_completes_after_observed_reconciliation'] = done['status'] == 'completed' and next(n.get('output') for n in done['nodes'] if n['id'] == 'result') == {'saved': True}
            api('/workflows/skill-note/resume', {})
            report['checks']['terminal_reentry_unchanged_no_duplicate_write'] = workflow(api, 'skill-note') == done and operator_status(endpoint, token)['result']['structuredContent']['entries'] == 1
        report['completed'] = True
    except Exception as error:
        report['failures'].append(str(error))
    finally:
        if service is not None:
            service.terminate()
            try:
                service.wait(timeout=10)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait(timeout=10)
        if log is not None:
            log.close()
    for check, passed in report['checks'].items():
        if not passed:
            report['failures'].append('check failed: ' + check)
    report['status'] = 'failed' if report['failures'] else 'passed'
    report['wall_ms'] = round((time.perf_counter() - started) * 1000)
    save(root / 'report.json', report)
    print(json.dumps({k: report[k] for k in ['status', 'completed', 'failures', 'checks', 'wall_ms']}), flush=True)
    raise SystemExit(1 if report['failures'] else 0)


if __name__ == '__main__':
    main()
