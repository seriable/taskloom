"""Shared visible native-role adapter for explicitly qualified framework trials.

The framework owns role scheduling. This harness owns a zero-fee local model
transport, saved raw traffic and the same Taskloom evidence verifier used by
the reference campaign. It does not attest arbitrary imported evidence, supply
model answers, or implement production authorization/crash recovery.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit

from native_retrieval import save


def compact(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


class NativeRoles:
    def __init__(self, config, suite, directory, api, verifier, reference):
        if (config['provider']['type'] != 'openai'
                or urlsplit(config['provider']['base_url']).hostname not in {'127.0.0.1', 'localhost', '::1'}
                or not config.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in config['prices'])):
            raise ValueError('only explicitly zero-priced loopback native model APIs are supported')
        self.config, self.directory, self.api, self.verifier = config, directory, api, verifier
        self.parent_inputs = {c['id']: c['observation']['state']['input'] for c in reference['cases']}
        self.roles = {r['name']: r for r in suite['roles']}
        self.tools = {}
        for case in reference['cases']:
            for child in case['children']:
                for tool in child['config']['tools']:
                    if tool['name'] != 'knowledge_search' or tool['effect'] != 'read_only':
                        raise ValueError('this adapter supports only the shared read-only knowledge tool')
                    if tool['name'] in self.tools and self.tools[tool['name']] != tool:
                        raise ValueError('reference contains incompatible tool definitions')
                    self.tools[tool['name']] = tool
        if set(self.tools) != {'knowledge_search'}:
            raise ValueError('reference has no exact shared knowledge contract')
        self.records = []
        self.guard = threading.Lock()
        self.active_analysts = self.maximum_concurrent_analysts = 0
        self.request_count = 0
        self.started = time.monotonic()

    def role(self, case, node, role, *, dependencies=None, params=None):
        directory = self.directory / node
        directory.mkdir(exist_ok=False)
        run_id = 'workflow-' + hashlib.sha256(compact([case['id'], node]).encode()).hexdigest()
        context = {'workflow_id': case['id'], 'node_id': node, 'run_id': run_id,
                   'input': self.parent_inputs[case['id']],
                   'params': params, 'dependencies': dependencies or {}}
        role_config = self.roles[role]
        messages = [{'role': 'system', 'content': role_config['system']}, {'role': 'user', 'content': compact(context)}]
        record = {'node': node, 'role': role, 'messages': messages, 'requests': [], 'tool_steps': [], 'events': [],
                  'usage': {'requests': 0, 'input_tokens': 0, 'output_tokens': 0, 'cost_micros': 0},
                  'input_context': context, 'max_steps': 16, 'max_requested_tool_calls': 32,
                  'status': 'running', 'start_monotonic': time.monotonic(), 'repairs': 0}
        with self.guard:
            self.records.append(record)
            if role == 'analyst':
                self.active_analysts += 1
                self.maximum_concurrent_analysts = max(self.maximum_concurrent_analysts, self.active_analysts)
        path = directory / 'native.json'

        def event(kind):
            record['events'].append({'sequence': len(record['events']), 'elapsed_ms': round((time.monotonic() - record['start_monotonic']) * 1000),
                                     'kind': kind})

        def run_report():
            # The verifier consumes the actual observed tool dispatch journal;
            # it never receives evidence copied from a dependency as a tool result.
            return {'run_id': run_id, 'messages': messages, 'stop_reason': 'completed', 'detail': None,
                    'steps': len(record['requests']), 'tool_calls': len(record['tool_steps']),
                    'usage': record['usage'], 'budget': {'generation': 0, 'limit_micros': 0, 'committed_micros': 0,
                        'in_flight_micros': 0, 'in_flight_attempts': 0, 'uncertain_micros': 0, 'uncertain_attempts': 0, 'denial': None},
                    'elapsed_ms': round((time.monotonic() - record['start_monotonic']) * 1000), 'events': record['events'], 'contexts': []}

        try:
            for step in range(1, 17):
                if time.monotonic() - self.started > 300:
                    raise RuntimeError('shared 300-second case deadline reached')
                tools = [self.tools[n] for n in role_config['tools']]
                if record['repairs'] and role_config['output']['type'] != 'validated':
                    tools = []
                body = {'model': self.config['model'], 'messages': provider_messages(messages), 'temperature': 0.0,
                        'max_tokens': self.config['output_tokens']}
                if tools:
                    body['tools'] = [{'type': 'function', 'function': {'name': t['name'], 'description': t['description'],
                                     'parameters': t['input_schema']}} for t in tools]
                with self.guard:
                    if self.request_count >= 256:
                        raise RuntimeError('bounded shared campaign request ceiling reached')
                    self.request_count += 1
                attempt = {'request': json.loads(compact(body)), 'reservation': {'currency': 'USD', 'ceiling_micros': 0,
                           'persisted_before_http_dispatch': True}, 'outcome': 'pending'}
                record['requests'].append(attempt)
                save(path, record)
                request = urllib.request.Request(self.config['provider']['base_url'] + '/chat/completions',
                    data=compact(body).encode(), headers={'Content-Type': 'application/json',
                    'Authorization': 'Bearer ' + os.environ[self.config['provider']['key_env']]})
                # One observed HTTP attempt per decision. Unlike the reference
                # gateway this transport does not retry transient HTTP failures.
                class NoRedirect(urllib.request.HTTPRedirectHandler):
                    def redirect_request(self, *unused):
                        return None
                remaining = 300 - (time.monotonic() - self.started)
                if remaining <= 0:
                    raise RuntimeError('case deadline reached before HTTP dispatch')
                with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect()).open(request, timeout=min(120, remaining)) as response:
                    raw = json.loads(response.read(2 * 1024 * 1024))
                attempt.update(response=raw, outcome='received')
                usage = raw.get('usage')
                if not usage:
                    raise RuntimeError('native usage missing; no fabricated token totals')
                record['usage']['requests'] += 1
                record['usage']['input_tokens'] += usage['prompt_tokens']
                record['usage']['output_tokens'] += usage['completion_tokens']
                save(path, record)
                choice = raw['choices'][0]
                if choice['finish_reason'] not in {'stop', 'tool_calls'}:
                    raise RuntimeError('native output truncated/refused')
                turn = choice['message']
                calls = turn.get('tool_calls') or []
                assistant = {'role': 'assistant', 'turn': {'content': turn.get('content') or '', 'tool_calls': [
                    {'id': c['id'], 'name': c['function']['name'], 'arguments': json.loads(c['function']['arguments'])} for c in calls]}}
                # Keep SDK-shaped observed history for its strict verifier.
                messages.append(assistant)
                if calls:
                    if len(record['tool_steps']) + len(calls) > 32:
                        raise RuntimeError('requested tool-call ceiling reached')
                    import jsonschema
                    for call in assistant['turn']['tool_calls']:
                        if call['name'] not in {t['name'] for t in tools}:
                            raise RuntimeError('tool call outside the frozen role grant')
                        jsonschema.validate(call['arguments'], self.tools[call['name']]['input_schema'])
                    for call in assistant['turn']['tool_calls']:
                        event({'type': 'tool_started', 'call_id': call['id'], 'name': call['name']})
                        record['tool_steps'].append(call)
                        save(path, record)
                        result = self.api('/knowledge/search', {'text': call['arguments']['query'],
                                          'top_k': call['arguments'].get('top_k', 5), 'max_bytes': 24 * 1024})
                        messages.append({'role': 'tool', 'result': {'call_id': call['id'], 'output': result, 'is_error': False}})
                        event({'type': 'tool_finished', 'call_id': call['id'], 'is_error': False})
                    save(path, record)
                    continue
                output = role_config['output']
                if output['type'] == 'validated':
                    check = subprocess.run([str(self.verifier)], input=compact({'policy': output['policy'], 'run': run_report()}).encode(),
                                           capture_output=True, timeout=10)
                    if check.returncode:
                        error = check.stderr.decode(errors='replace')[:1024]
                        if record['repairs'] >= output['policy'].get('max_repairs', 0):
                            raise RuntimeError(error)
                        record['repairs'] += 1
                        messages.append({'role': 'user', 'content': 'Final output rejected by the host: ' + error + '. Correct the output using permitted retrieval only.'})
                        continue
                    value = json.loads(check.stdout)
                else:
                    import jsonschema
                    value = json.loads(assistant['turn']['content'])
                    jsonschema.validate(value, output['schema'])
                record.update(status='completed', output=value)
                return value
            raise RuntimeError('model-step ceiling reached')
        except Exception as error:
            record.update(status='failed', failure=str(error))
            for attempt in record['requests']:
                if attempt['outcome'] == 'pending':
                    attempt['outcome'] = 'unknown'
            raise
        finally:
            record['wall_ms'] = round((time.monotonic() - record['start_monotonic']) * 1000)
            record['native_usage_complete'] = all(a['outcome'] == 'received' and isinstance(a.get('response', {}).get('usage'), dict)
                and all(k in a['response']['usage'] for k in ['prompt_tokens', 'completion_tokens']) for a in record['requests'])
            save(path, record)
            with self.guard:
                if role == 'analyst':
                    self.active_analysts -= 1


def provider_messages(messages):
    """Explicit SDK-to-OpenAI mapping; raw native responses remain untouched."""
    result = []
    for message in messages:
        role = message['role']
        if role == 'assistant':
            turn = message['turn']
            item = {'role': role, 'content': turn['content'] or None}
            if turn['tool_calls']:
                item['tool_calls'] = [{'id': c['id'], 'type': 'function', 'function': {'name': c['name'], 'arguments': compact(c['arguments'])}} for c in turn['tool_calls']]
            result.append(item)
        elif role == 'tool':
            observation = message['result']
            result.append({'role': role, 'tool_call_id': observation['call_id'], 'content': compact(observation['output'])})
        else:
            result.append(message)
    return result
