"""Owned loopback-only zero-fee native recorder for framework comparisons.

Records reservations before dispatch and keeps unknown responses visible. This
is bounded evaluation plumbing, not a production paid-account ledger. Only the
configured model and read-only shared knowledge callback can be contacted.
"""
import contextlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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


def verifier_envelope(observations, text):
    """SDK validator bridge from recorded calls, never a native Taskloom run."""
    messages=[]; events=[]
    for hit in observations:
        if hit['status']!='completed': continue
        events.extend([{'sequence':len(events),'elapsed_ms':0,'kind':{'type':'tool_started','call_id':hit['call_id'],'name':'knowledge_search'}},
                       {'sequence':len(events)+1,'elapsed_ms':0,'kind':{'type':'tool_finished','call_id':hit['call_id'],'is_error':False}}])
        messages.append({'role':'tool','result':{'call_id':hit['call_id'],'output':hit['result'],'is_error':False}})
    messages.append({'role':'assistant','turn':{'content':text,'tool_calls':[]}})
    return {'run_id':'deerflow-validator-bridge','messages':messages,'stop_reason':'completed','detail':None,'steps':0,'tool_calls':len(observations),
        'usage':{'requests':0,'input_tokens':0,'output_tokens':0,'cost_micros':0},
        'budget':{'generation':0,'limit_micros':0,'committed_micros':0,'in_flight_micros':0,'in_flight_attempts':0,'uncertain_micros':0,'uncertain_attempts':0,'denial':None},
        'elapsed_ms':0,'events':events,'contexts':[]}


@contextlib.contextmanager
def relay(config, directory, api, verifier, policy):
    endpoint = urlsplit(config['provider']['base_url'])
    if (endpoint.scheme != 'http' or endpoint.hostname not in {'127.0.0.1', 'localhost', '::1'}
            or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment
            or not config.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in config['prices'])):
        raise ValueError('explicit zero-priced loopback model required')
    key = os.environ[config['provider']['key_env']]
    lock = threading.Lock()
    state = {'version':1,'model_requests':[],'knowledge':[],'maximum_overlapping_analyst_model_requests':0,
             'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'cap_micros':1},
             'limits':{'model_requests':64,'model_output_tokens':config['output_tokens'],'case_seconds':300}}
    active = 0
    started = time.monotonic()

    def persist():
        save(directory/'relay.json',state)

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self,*unused): return None

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect())

    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*unused): pass

        def reply(self,status,value):
            data=json.dumps(value,ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header('Content-Type','application/json')
            self.send_header('Content-Length',str(len(data)))
            self.end_headers()
            try: self.wfile.write(data)
            except (BrokenPipeError,ConnectionResetError): pass

        def do_POST(self):
            nonlocal active
            entry = None
            analyst = False
            try:
                if not hmac.compare_digest(self.headers.get('Authorization',''),'Bearer '+key):
                    return self.reply(401,{'error':'local evaluation credential required'})
                size=int(self.headers.get('Content-Length','0'))
                if not 0 < size <= 1024*1024:
                    return self.reply(413,{'error':'bounded JSON body required'})
                body=json.loads(self.rfile.read(size))
                if time.monotonic()-started >= 300:
                    return self.reply(429,{'error':'case deadline reached'})
                if self.path == '/validate-final':
                    if set(body) != {'text'} or not isinstance(body['text'],str):
                        raise ValueError('exact text input required')
                    with lock: evidence=json.loads(json.dumps(state['knowledge']))
                    checked=subprocess.run([str(verifier)],input=json.dumps({'policy':policy,'run':verifier_envelope(evidence,body['text'])}).encode(),
                                           capture_output=True,timeout=10)
                    value={'accepted':checked.returncode==0}
                    if checked.returncode: value['detail']=checked.stderr.decode(errors='replace')[:1500]
                    else: value['canonical']=json.loads(checked.stdout)
                    with lock:
                        state.setdefault('validation_attempts',[]).append({'text':body['text'],'result':value}); persist()
                    return self.reply(200,value)
                if self.path == '/knowledge':
                    if set(body) != {'call_id','query','top_k'}:
                        raise ValueError('invalid knowledge request')
                    with lock:
                        observed=[]
                        for attempt in state['model_requests']:
                            for choice in attempt.get('response',{}).get('choices',[]):
                                observed.extend(choice['message'].get('tool_calls') or [])
                        matches=[c for c in observed if c['id']==body['call_id'] and c['function']['name']=='knowledge_search'
                                 and json.loads(c['function']['arguments']).get('query')==body['query']
                                 and json.loads(c['function']['arguments']).get('top_k',5)==body['top_k']]
                        if not matches or any(k['call_id']==body['call_id'] for k in state['knowledge']):
                            raise ValueError('knowledge call lacks an exact observed native tool request or was already executed')
                        hit={'call_id':body['call_id'],'query':body['query'],'top_k':body['top_k'],'status':'pending'}
                        state['knowledge'].append(hit)
                        persist()
                    result=api('/knowledge/search',{'text':body['query'],'top_k':body['top_k'],'max_bytes':24576})
                    with lock:
                        hit.update(status='completed',result=result)
                        persist()
                    return self.reply(200,result)
                if self.path != '/v1/chat/completions':
                    return self.reply(404,{'error':'only chat completions and read-only knowledge are available'})
                if body.get('model') != config['model'] or body.get('stream'):
                    raise ValueError('model must match the explicit reference; configure non-streaming native calls')
                allowance=body.get('max_tokens',body.get('max_completion_tokens'))
                if not isinstance(allowance,int) or not 0 < allowance <= config['output_tokens']:
                    raise ValueError('native output allowance exceeds the reference')
                names={t.get('function',{}).get('name') for t in body.get('tools',[])}
                if not names <= {'knowledge_search','task'}:
                    raise ValueError('framework exposed tools outside the configured read-only/delegation grant: '+str(sorted(names)))
                systems=' '.join(str(m.get('content','')) for m in body['messages'] if m['role']=='system')
                role='analyst' if 'BENCHMARK_SOURCE_ANALYST' in systems else 'reviewer' if 'BENCHMARK_SOURCE_REVIEWER' in systems else 'lead'
                with lock:
                    if len(state['model_requests']) >= 64:
                        return self.reply(429,{'error':'model request ceiling reached'})
                    entry={'request':body,'role':role,'outcome':'pending','reservation_micros':0,
                           'reservation_written_before_dispatch':True,'start_ms':round((time.monotonic()-started)*1000)}
                    state['model_requests'].append(entry)
                    if role=='analyst':
                        analyst=True; active+=1
                        state['maximum_overlapping_analyst_model_requests']=max(active,state['maximum_overlapping_analyst_model_requests'])
                    persist()
                request=urllib.request.Request(config['provider']['base_url']+'/chat/completions',data=json.dumps(body).encode(),
                    headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})
                remaining=300-(time.monotonic()-started)
                if remaining <= 0: raise TimeoutError('case deadline reached before dispatch')
                with opener.open(request,timeout=min(120,remaining)) as response:
                    raw=response.read(2*1024*1024+1)
                    if len(raw)>2*1024*1024: raise ValueError('native response too large')
                    value=json.loads(raw)
                with lock:
                    entry.update(outcome='received',response=value)
                    persist()
                self.reply(200,value)
            except urllib.error.HTTPError as error:
                payload=error.read(16384).decode(errors='replace')
                if entry is not None:
                    with lock:
                        entry.update(outcome='http_error',status=error.code,body=payload); persist()
                self.reply(error.code,{'error':payload})
            except Exception as error:
                with lock:
                    if entry is not None: entry.update(outcome='unknown',error=str(error))
                    else: state.setdefault('rejected_requests',[]).append({'path':self.path,'error':str(error)})
                    persist()
                self.reply(400,{'error':str(error)})
            finally:
                with lock:
                    if analyst: active-=1
                    if entry is not None: entry['end_ms']=round((time.monotonic()-started)*1000)
                    persist()

    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    server.daemon_threads=False
    worker=threading.Thread(target=server.serve_forever,daemon=True)
    worker.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}',state
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)
        with lock: persist()
