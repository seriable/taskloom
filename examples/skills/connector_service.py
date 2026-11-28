"""Loopback MCP example: a host-pinned GitHub repo and an owned note directory.

No install hooks. Start explicitly, supply the local bearer environment, and
configure bindings/permissions in Taskloom. Fault injection is opt-in and
deliberately leaves an uncertain, non-idempotent append for reconciliation.
"""
import argparse
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import sys
import threading
import time
import urllib.request
import uuid


class Service:
    def __init__(self, args):
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,99}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}', args.repository):
            raise ValueError('choose one explicit owner/repository')
        self.token = os.environ[args.token_env]
        if len(self.token) < 16:
            raise ValueError('local bearer token must contain at least 16 characters')
        self.github_token = os.environ[args.github_token_env] if args.github_token_env else None
        self.root = args.notes.resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.repository = args.repository
        self.note_identity = hashlib.sha256(str(self.root).encode()).hexdigest()[:16]
        self.fail = args.fail_after_append_once
        self.fail_status_once = args.fail_first_status_once
        self.hold_seconds = args.hold_after_append_once_seconds
        if not 0 <= self.hold_seconds <= 30:
            raise ValueError('append hold must be between zero and 30 seconds')
        if self.fail and self.hold_seconds:
            raise ValueError('choose either response loss or response hold for one fault trial')
        self.mutex = threading.Lock()
        self.github_calls = 0
        self.github_observations = []
        self.sessions = set()

    def notes(self):
        path = self.root / 'notes.jsonl'
        if not path.exists():
            return []
        if path.stat().st_size > 1024 * 1024:
            raise ValueError('example note store exceeds 1 MiB')
        return [json.loads(line) for line in path.read_text(encoding='utf-8').splitlines()]

    def metadata(self):
        with self.mutex:
            if self.github_calls >= 20:
                raise ValueError('explicit example GitHub request cap reached')
            self.github_calls += 1
        headers = {'Accept': 'application/vnd.github+json', 'User-Agent': 'taskloom-source-connector-example',
                   'X-GitHub-Api-Version': '2022-11-28'}
        if self.github_token:
            headers['Authorization'] = 'Bearer ' + self.github_token
        request = urllib.request.Request('https://api.github.com/repos/' + self.repository, headers=headers)
        # No environment proxy, redirects or implicit token discovery.
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *unused):
                return None
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        try:
            with opener.open(request, timeout=10) as response:
                raw = response.read(512 * 1024 + 1)
                if len(raw) > 512 * 1024:
                    raise ValueError('GitHub response exceeds allowance')
                body = json.loads(raw)
                if body['full_name'].lower() != self.repository.lower():
                    raise ValueError('GitHub returned a different repository')
                result = {key: body[key] for key in ['full_name', 'description', 'default_branch', 'html_url', 'stargazers_count', 'open_issues_count']}
                result['license'] = (body.get('license') or {}).get('spdx_id') or 'unknown'
                remaining = response.headers.get('X-RateLimit-Remaining')
        except Exception as error:
            with self.mutex:
                self.github_observations.append({'error_type': type(error).__name__, 'detail': str(error)[:512]})
                self.save_observations()
            raise
        with self.mutex:
            self.github_observations.append({'result': result, 'rate_limit_remaining': remaining})
            self.save_observations()
        return result

    def save_observations(self):
        # Contains public metadata and counts only, never request headers/tokens.
        (self.root / 'observations.json').write_text(json.dumps({'repository': self.repository,
            'github_requests': self.github_calls, 'observations': self.github_observations,
            'service_fees': {'currency': 'USD', 'public_api_price_per_call': 0,
                             'local_hardware_electricity_unmetered': True}}, indent=2) + '\n', encoding='utf-8')


def tools(service):
    return [
        {'name': 'repo_metadata', 'description': 'Read metadata for the repository pinned by the host: ' + service.repository,
         'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}},
        {'name': 'notes_status', 'description': 'Read the count and latest entries from owned note-store ' + service.note_identity,
         'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}},
        {'name': 'notes_append', 'description': 'Append one note to owned note-store ' + service.note_identity + '. This is a non-idempotent write; do not retry an uncertain outcome.',
         'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string', 'minLength': 1, 'maxLength': 2048}},
                         'required': ['text'], 'additionalProperties': False}}]


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, unused, *args):
        pass

    def respond(self, status, body=None, session=None):
        encoded = b'' if body is None else json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(encoded)))
        if session:
            self.send_header('Mcp-Session-Id', session)
        self.end_headers()
        self.wfile.write(encoded)

    def do_POST(self):
        service = self.server.service
        supplied = self.headers.get('Authorization', '')
        if self.path != '/mcp' or not hmac.compare_digest(supplied, 'Bearer ' + service.token):
            self.respond(403)
            self.close_connection = True
            return
        length = int(self.headers.get('Content-Length', '0'))
        if not 1 <= length <= 16384:
            self.respond(413)
            self.close_connection = True
            return
        body = json.loads(self.rfile.read(length))
        identity = body.get('id')
        method = body.get('method')
        session = self.headers.get('Mcp-Session-Id')
        if method == 'initialize':
            session = uuid.uuid4().hex
            with service.mutex:
                service.sessions.add(session)
            self.respond(200, {'jsonrpc': '2.0', 'id': identity, 'result': {'protocolVersion': '2025-06-18',
                'capabilities': {'tools': {}}, 'serverInfo': {'name': 'taskloom-notes-github-example', 'version': '1.0.0'}}}, session)
            return
        if session not in service.sessions or self.headers.get('MCP-Protocol-Version') != '2025-06-18':
            self.respond(404)
            return
        if method == 'notifications/initialized':
            self.respond(202)
            return
        if method == 'tools/list':
            self.respond(200, {'jsonrpc': '2.0', 'id': identity, 'result': {'tools': tools(service)}})
            return
        try:
            if method != 'tools/call':
                raise ValueError('unsupported method')
            params = body['params']
            arguments = params.get('arguments', {})
            if params['name'] == 'repo_metadata' and arguments == {}:
                result = service.metadata()
            elif params['name'] == 'notes_status' and arguments == {}:
                with service.mutex:
                    marker = service.root / 'status-read-failed-once'
                    if service.fail_status_once and not marker.exists():
                        with marker.open('xb') as stream:
                            stream.write(b'explicit read error returned once; no note was written\n')
                            stream.flush()
                            os.fsync(stream.fileno())
                        raise ValueError('Explicit injected read failure. No status is known from this call. A new read-only notes_status request is safe; do not guess the result.')
                    notes = service.notes()
                    result = {'entries': len(notes), 'latest': notes[-5:]}
            elif params['name'] == 'notes_append' and set(arguments) == {'text'} and isinstance(arguments['text'], str) and 1 <= len(arguments['text']) <= 2048:
                hold_reply_seconds = 0
                with service.mutex:
                    notes = service.notes()
                    if len(notes) >= 100:
                        raise ValueError('explicit example note-count cap reached')
                    with (service.root / 'notes.jsonl').open('ab') as stream:
                        stream.write((json.dumps({'text': arguments['text']}, ensure_ascii=False) + '\n').encode())
                        stream.flush()
                        os.fsync(stream.fileno())
                    result = {'saved': True, 'entries': len(notes) + 1}
                    hold_marker = service.root / 'held-after-append'
                    if service.hold_seconds and not hold_marker.exists():
                        with hold_marker.open('xb') as stream:
                            stream.write(b'append fsynced; response held for owned caller process-death acceptance\n')
                            stream.flush()
                            os.fsync(stream.fileno())
                        hold_reply_seconds = service.hold_seconds
                    marker = service.root / 'fault-injected'
                    if service.fail and not marker.exists():
                        with marker.open('xb') as stream:
                            stream.write(b'append persisted before response loss\n')
                            stream.flush()
                            os.fsync(stream.fileno())
                        self.close_connection = True
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                # Hold only this caller's reply, never the session/notes lock:
                # a second host must still initialize and inspect real state.
                if hold_reply_seconds:
                    time.sleep(hold_reply_seconds)
            else:
                raise ValueError('unknown tool or invalid arguments')
            response = {'content': [{'type': 'text', 'text': json.dumps(result)}], 'structuredContent': result, 'isError': False}
        except Exception as error:
            # Error text is a protocol observation, never permission or retry.
            response = {'content': [{'type': 'text', 'text': type(error).__name__ + ': ' + str(error)}], 'isError': True}
        self.respond(200, {'jsonrpc': '2.0', 'id': identity, 'result': response})

    def do_DELETE(self):
        service = self.server.service
        supplied = self.headers.get('Authorization', '')
        if self.path != '/mcp' or not hmac.compare_digest(supplied, 'Bearer ' + service.token):
            self.respond(403)
            self.close_connection = True
            return
        session = self.headers.get('Mcp-Session-Id')
        with service.mutex:
            if session not in service.sessions or self.headers.get('MCP-Protocol-Version') != '2025-06-18':
                self.respond(404)
                return
            service.sessions.remove(session)
        # Termination affects future requests, not a past uncertain append.
        self.respond(204)


class ExampleServer(ThreadingHTTPServer):
    def handle_error(self, request, client_address):
        if isinstance(sys.exc_info()[1], (ConnectionResetError, BrokenPipeError)):
            return
        super().handle_error(request, client_address)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--notes', type=Path, required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--token-env', default='TASKLOOM_EXAMPLE_CONNECTOR_TOKEN')
    parser.add_argument('--github-token-env')
    parser.add_argument('--port', type=int, default=18084)
    parser.add_argument('--ready-file', type=Path)
    parser.add_argument('--fail-after-append-once', action='store_true')
    parser.add_argument('--fail-first-status-once', action='store_true',
                        help='return one explicit read error, then normal status; default disabled')
    parser.add_argument('--hold-after-append-once-seconds', type=float, default=0,
                        help='explicit 0..30 second fault-test hold after first fsynced append; default disabled')
    args = parser.parse_args()
    service = Service(args)
    server = ExampleServer(('127.0.0.1', args.port), Handler)
    server.service = service
    server.daemon_threads = True
    address = {'endpoint': f'http://127.0.0.1:{server.server_port}/mcp', 'repository': args.repository,
               'notes_directory': str(service.root)}
    if args.ready_file:
        with args.ready_file.open('x', encoding='utf-8') as stream:
            json.dump(address, stream)
    print(json.dumps(address), flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
