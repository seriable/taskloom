"""Bound a native campaign to an explicitly selected prepaid token-plan route.

Pass-through transport only: no generated answers, prompt rewriting, model
fallback or upstream retries. Each HTTP attempt consumes a durable allocation
before dispatch, including errors and unknown results. The finite allocation
is independent of the application's token-cost journal and the provider's
monthly billing counter. Subscription price/remaining allowance are unknown.
"""
import argparse
import hashlib
import hmac
import http.server
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import threading
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit
import uuid

ALLOWED_BASE = 'https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1'
METADATA_ENV = 'TASKLOOM_NATIVE_SUBSCRIPTION_METADATA'


def write_json(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('x', encoding='utf-8') as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write('\n'); output.flush(); os.fsync(output.fileno())
    os.replace(temporary, path)


def metadata_for(config):
    path = os.environ.get(METADATA_ENV)
    if not path:
        return None
    value = json.loads(Path(path).read_text(encoding='utf-8'))
    if (value['upstream_base_url'] != ALLOWED_BASE or value['model'] != config['model']
            or value['local_provider'] != config['provider']
            or urlsplit(config['provider']['base_url']).hostname != '127.0.0.1'):
        raise ValueError('subscription transport metadata/configuration mismatch')
    return value


class RequestLedger:
    def __init__(self, path, limit):
        if type(limit) is not int or not 1 <= limit <= 1000:
            raise ValueError('finite request allocation must be 1..1000')
        self.path = path
        self.limit = limit
        self.lock = threading.Lock()
        self.count = 0
        # Fresh campaigns only. A crashed allocation must never be reset by
        # reopening the old path; retain its unresolved reservations instead.
        with path.open('x', encoding='utf-8') as output:
            output.write(''); output.flush(); os.fsync(output.fileno())

    def _append(self, value):
        with self.path.open('a', encoding='utf-8') as output:
            output.write(json.dumps(value, ensure_ascii=False)+'\n')
            output.flush(); os.fsync(output.fileno())

    def reserve(self, request_hash):
        with self.lock:
            if self.count >= self.limit:
                return None
            ordinal = self.count + 1
            self._append({'ordinal': ordinal, 'event': 'reserved_before_dispatch',
                          'request_sha256': request_hash, 'unix': time.time()})
            self.count = ordinal
            return ordinal

    def settle(self, ordinal, **value):
        with self.lock:
            self._append({'ordinal': ordinal, 'event': 'observation', **value})


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def make_handler(ledger, directory, model, remote_key, local_token):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, status, body):
            try:
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except ConnectionError:
                # A caller closing its socket does not undo known upstream
                # usage. Runtime-side uncertainty is recorded independently.
                pass

        def do_POST(self):
            if (self.path != '/v1/chat/completions' or not hmac.compare_digest(
                    self.headers.get('Authorization', ''), 'Bearer '+local_token)):
                return self.reply(403, b'{"error":{"message":"local transport binding refused"}}')
            try:
                length = int(self.headers.get('Content-Length', '0'))
                if not 0 < length <= 2*1024*1024:
                    raise ValueError('body bound')
                body = self.rfile.read(length)
                payload = json.loads(body)
                if payload.get('model') != model or payload.get('stream', False):
                    raise ValueError('model/stream binding')
                if remote_key.encode() in body:
                    raise ValueError('credential found in model body')
            except (ValueError, TypeError, AttributeError):
                return self.reply(400, b'{"error":{"message":"bounded nonstreaming model request required"}}')
            ordinal = ledger.reserve(hashlib.sha256(body).hexdigest())
            if ordinal is None:
                return self.reply(403, b'{"error":{"message":"native request allocation exhausted; no upstream dispatch"}}')
            started = time.monotonic()
            try:
                # Evidence writing is also before dispatch. A write error never
                # sends an unrecorded request; its reservation remains occupied.
                write_json(directory/f'request-{ordinal}.json', payload)
                request = urllib.request.Request(ALLOWED_BASE+'/chat/completions', data=body,
                    headers={'Content-Type':'application/json','Authorization':'Bearer '+remote_key})
                with urllib.request.build_opener(NoRedirect()).open(request, timeout=120) as response:
                    status = response.status; raw = response.read(4*1024*1024+1)
                if len(raw) > 4*1024*1024:
                    raise ValueError('upstream response exceeded bound; usage unknown')
                parsed = json.loads(raw)
                safe = raw.replace(remote_key.encode(), b'[REDACTED]')
                (directory/f'response-{ordinal}.json').write_bytes(safe)
                ledger.settle(ordinal, status='response_received', http_status=status,
                              model=parsed.get('model'), usage=parsed.get('usage'),
                              wall_ms=round((time.monotonic()-started)*1000))
                self.reply(status, safe)
            except urllib.error.HTTPError as error:
                with error:
                    raw = error.read(65536).replace(remote_key.encode(), b'[REDACTED]')
                (directory/f'error-{ordinal}.json').write_bytes(raw)
                ledger.settle(ordinal, status='http_error_usage_not_reported', http_status=error.code,
                              wall_ms=round((time.monotonic()-started)*1000))
                self.reply(error.code, raw)
            except Exception as error:
                ledger.settle(ordinal, status='uncertain_after_reservation',
                              error=str(error).replace(remote_key, '[REDACTED]'),
                              wall_ms=round((time.monotonic()-started)*1000))
                self.reply(502, b'{"error":{"message":"native transport failed; reserved attempt retained"}}')
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--max-requests', type=int, required=True)
    parser.add_argument('--monthly-calls', type=int, required=True)
    parser.add_argument('--campaign', choices=['collaboration', 'claim-support', 'long-session','long-recovery'], required=True)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--verifier', type=Path)
    parser.add_argument('--suite', type=Path)
    parser.add_argument('--selected-sources', action='store_true')
    parser.add_argument('--separate-evidence', action='store_true')
    parser.add_argument('--strict-length', action='store_true')
    parser.add_argument('--controls', type=Path)
    parser.add_argument('--grounded', action='store_true')
    parser.add_argument('--rubric', choices=['v1','v2','v3'], default='v1')
    parser.add_argument('--assessment', action='store_true')
    parser.add_argument('--recover-from',type=Path)
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding='utf-8'))
    provider = config['provider']
    if (provider.get('type') != 'openai' or provider.get('base_url') != ALLOWED_BASE
            or not config['model'] or not provider.get('key_env')
            or not 1 <= args.max_requests <= min(1000, args.monthly_calls)
            or args.campaign not in ['long-session','long-recovery'] and args.verifier is None
            or args.selected_sources and args.campaign != 'long-session'
            or args.strict_length and args.campaign != 'long-session'
            or args.grounded and args.campaign != 'long-session'
            or args.rubric != 'v1' and args.campaign != 'claim-support'
            or args.assessment and (args.campaign != 'claim-support' or not args.separate_evidence)
            or args.controls is not None and args.campaign != 'claim-support'
            or args.separate_evidence and args.campaign != 'claim-support'
            or args.suite is not None and args.campaign == 'claim-support'):
        raise ValueError('explicit selected token-plan route/model/credential and finite allocation required')
    if (args.recover_from is None)!=(args.campaign!='long-recovery'):
        raise ValueError('long-recovery requires an explicit previous failed campaign')
    key = os.environ[provider['key_env']]
    if not key:
        raise ValueError('empty remote credential')
    root = args.out.resolve(); root.mkdir(parents=True, exist_ok=False)
    source_directory = root/'source'; source_directory.mkdir()
    identities = []
    for source in [Path(__file__), Path(__file__).with_name('native_retrieval.py'),
                   Path(__file__).with_name({'collaboration':'native_collaboration.py',
                       'claim-support':'native_claim_support.py','long-session':'native_long_session.py',
                       'long-recovery':'native_long_session_recover.py'}[args.campaign])]:
        shutil.copy2(source, source_directory/source.name)
        identities.append({'path':str(source.resolve()),'sha256':hashlib.sha256(source.read_bytes()).hexdigest()})
    write_json(root/'source-identities.json', identities)
    ledger = RequestLedger(root/'requests.jsonl', args.max_requests)
    directory = root/'transport'; directory.mkdir()
    token = uuid.uuid4().hex
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), make_handler(ledger,directory,config['model'],key,token))
    proxy_provider = {'type':'openai', 'base_url':f'http://127.0.0.1:{server.server_port}/v1',
                      'namespace':provider['namespace'], 'key_env':'TASKLOOM_NATIVE_PROXY_TOKEN'}
    metadata = {'evidence_kind':'actual remote native model API via unchanged pass-through transport',
                'upstream_base_url':ALLOWED_BASE, 'model':config['model'], 'credential_environment':provider['key_env'],
                'local_provider':proxy_provider, 'monthly_calls_user_declared':args.monthly_calls,
                'max_remote_http_attempts':args.max_requests, 'invoice_verified':False,
                'provider_remaining_allowance':None,
                'fees':'Prepaid subscription declared by user. Token meter zero represents configured additional cash charge within this route, never subscription price or a public free token tariff. No invoice/overage policy is verified; no other endpoint or model fallback is permitted.',
                'quota':'Locally counted upstream HTTP attempts; provider billing multipliers and monthly balance are unknown. Reservations are durable before dispatch; errors/unknown usage never refund request allocation.'}
    write_json(root/'subscription.json',metadata)
    config.update(provider=proxy_provider, prices=[{'prefix':config['model'],'input_per_million':0,'output_per_million':0}])
    write_json(root/'model.json',config)
    env=dict(os.environ)
    env.pop(provider['key_env'],None)
    env.update(TASKLOOM_NATIVE_PROXY_TOKEN=token, **{METADATA_ENV:str(root/'subscription.json')})
    name = {'collaboration':'native_collaboration.py','claim-support':'native_claim_support.py','long-session':'native_long_session.py','long-recovery':'native_long_session_recover.py'}[args.campaign]
    command=[sys.executable,str(Path(__file__).with_name(name)),str(root/'campaign'),
             '--config',str(root/'model.json'),'--taskloom',str(args.taskloom.resolve())]
    if args.verifier: command += ['--verifier',str(args.verifier.resolve())]
    if args.suite: command += ['--suite',str(args.suite.resolve())]
    if args.selected_sources: command.append('--selected-sources')
    if args.separate_evidence: command.append('--separate-evidence')
    if args.strict_length: command.append('--strict-length')
    if args.grounded: command.append('--grounded')
    if args.campaign=='claim-support':command += ['--rubric',args.rubric]
    if args.assessment:command.append('--assessment')
    if args.recover_from:command += ['--recover-from',str(args.recover_from.resolve())]
    if args.controls:
        command += ['--controls',str(args.controls.resolve())]
    thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
    result={'scope':__doc__, 'subscription':metadata,'command':command,'completed':False}
    write_json(root/'report.json',result)
    try:
        finished=subprocess.run(command,env=env)
        result.update(completed=True,returncode=finished.returncode)
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=5)
        result['reserved_http_attempts']=ledger.count
        write_json(root/'report.json',result)
    raise SystemExit(finished.returncode)


if __name__ == '__main__':
    main()
