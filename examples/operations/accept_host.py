"""Real local SQL service health/authority and SIGTERM smoke; no model response.

Uses a fresh owned SQLite file and a counting loopback provider that refuses
every request. Passing requires zero provider requests, not scripted answers.
On Windows the owned idle process is terminated explicitly; only POSIX proves
graceful SIGTERM. This is deployment plumbing, never an effectiveness/SLO test.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--taskloom', required=True, type=Path)
    parser.add_argument('--admin', type=Path, help='optional trusted administration binary for private-memory route smoke')
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    executable = args.taskloom.resolve()
    count = []
    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass
        def do_POST(self):
            count.append(self.path)
            self.send_response(503)
            self.end_headers()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    environment = dict(os.environ)
    environment['TASKLOOM_HOST_SMOKE_DATABASE'] = 'sqlite://'+(root/'service.sqlite').as_posix()+'?mode=rwc'
    environment['TASKLOOM_DATABASE_URL'] = environment['TASKLOOM_HOST_SMOKE_DATABASE']
    environment['TASKLOOM_HOST_SMOKE_KEY'] = 'disposable-'+uuid.uuid4().hex
    def cli(*arguments, env=None, accepted=0):
        run = subprocess.run([str(executable), *map(str, arguments)], env=env or environment,
                             capture_output=True, timeout=30)
        if run.returncode != accepted:
            raise RuntimeError('command failed: '+str(arguments)+': '+run.stderr.decode(errors='replace')[:2000])
        return run
    def save(name, value):
        (root/name).write_text(json.dumps(value, indent=2)+'\n', encoding='utf-8')
    def request(path, token=None, body=None):
        try:
            headers = {} if token is None else {'Authorization':'Bearer '+token}
            raw = None if body is None else json.dumps(body).encode()
            if raw is not None: headers['Content-Type']='application/json'
            req = urllib.request.Request('http://127.0.0.1:'+str(port)+path, data=raw, headers=headers)
            with urllib.request.urlopen(req, timeout=5) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, error.read(4096).decode(errors='replace')
    process = None
    report = {'scope': 'Actual native host/SQLite/no-dispatch and schema failure smoke, not model quality or production load.',
              'binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(), 'checks': {}}
    log = (root/'host.log').open('wb')
    def start():
        child = subprocess.Popen([str(executable),'service-serve',str(root/'host.json')],env=environment,
                                 stdout=log,stderr=log,
                                 creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
        for _ in range(100):
            if child.poll() is not None:
                raise RuntimeError('owned service exited before ready')
            try:
                if request('/health/ready')[0] == 200:
                    return child
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(.05)
        child.terminate()
        child.wait(timeout=15)
        raise RuntimeError('owned service did not become ready')
    def stop(child):
        child.send_signal(signal.SIGTERM) if os.name != 'nt' else child.terminate()
        child.wait(timeout=15)
    try:
        cli('init', root/'project.json')
        config = json.loads((root/'project.json').read_text(encoding='utf-8'))
        config.update(state_dir=str(root/'state'), knowledge=False, model='not-called',
            provider={'type':'openai', 'base_url':'http://127.0.0.1:'+str(provider.server_port)+'/v1',
                      'key_env':'TASKLOOM_HOST_SMOKE_KEY', 'namespace':'owned-host-smoke'},
            database={'url_env':'TASKLOOM_HOST_SMOKE_DATABASE', 'tenant':'smoke', 'project':'main'},
            prices=[{'prefix':'not-called','input_per_million':1.0,'output_per_million':1.0}],
            limits=dict(config['limits'], max_cost_micros=1000))
        save('project.json', config)
        save('host.json', {'listen':'127.0.0.1:'+str(port), 'database_url_env':'TASKLOOM_HOST_SMOKE_DATABASE',
                          'projects':[{'tenant':'smoke','project':'main','config':'project.json'}], 'shutdown_grace_seconds':5})
        save('queue.json', {'max_open_jobs':4,'max_running':2})
        cli('--config',root/'project.json','database-migrate')
        refused = subprocess.run([str(executable),'service-check',str(root/'host.json')],env=environment,capture_output=True)
        report['checks']['unconfigured_queue_refuses_readiness'] = refused.returncode != 0
        cli('--config',root/'project.json','queue-configure',root/'queue.json','0','smoke','explicit owned check')
        tokens = {}
        if args.admin:
            admin = args.admin.resolve()
            for revision, name in enumerate(['alice','bob']):
                save(name+'.json', {'id':name,'enabled':True,'memberships':[{'tenant':'smoke','project':'main','role':'operator'}]})
                put = subprocess.run([str(admin),'put-user',str(root/(name+'.json')),str(revision*2),'smoke'],env=environment,capture_output=True)
                if put.returncode: raise RuntimeError('owned identity creation failed: '+put.stderr.decode(errors='replace'))
                issued = subprocess.run([str(admin),'issue',name,str(root/(name+'.private-token')),'smoke'],env=environment,capture_output=True)
                if issued.returncode: raise RuntimeError('owned token issuance failed')
                tokens[name] = (root/(name+'.private-token')).read_text(encoding='utf-8')
        cold = dict(environment)
        cold.pop('TASKLOOM_HOST_SMOKE_KEY')
        cli('service-check',root/'host.json',env=cold)
        report['checks']['cold_check_needs_no_model_credential'] = True
        process = start()
        report['checks']['ready_checks_real_database_and_queue'] = request('/health/ready') == (200, {'ready':True})
        report['checks']['liveness_is_separate'] = request('/health/live') == (200, {'live':True})
        report['checks']['project_api_requires_bearer'] = request('/tenants/smoke/projects/main/api/queue')[0] == 401
        report['checks']['single_operator_root_not_mounted'] = request('/api/runs')[0] == 404
        cli('health-check','http://127.0.0.1:'+str(port)+'/health/ready')
        wrong = subprocess.run([str(executable),'health-check','http://example.com/health/ready'],capture_output=True)
        report['checks']['probe_refuses_remote_host'] = wrong.returncode != 0
        # A real owned schema change makes readiness fail while liveness stays
        # available; restore the exact version afterward. No work is admitted.
        with sqlite3.connect(root/'service.sqlite') as db:
            version = db.execute('SELECT version FROM taskloom_meta WHERE singleton=1').fetchone()[0]
            db.execute('UPDATE taskloom_meta SET version=999 WHERE singleton=1')
        try:
            report['checks']['schema_failure_ready_503_live_200'] = request('/health/ready')[0] == 503 and request('/health/live')[0] == 200
        finally:
            with sqlite3.connect(root/'service.sqlite') as db:
                db.execute('UPDATE taskloom_meta SET version=? WHERE singleton=1',(version,))
        report['checks']['readiness_recovers_without_model_request'] = request('/health/ready')[0] == 200
        if tokens:
            memory = '/tenants/smoke/projects/main/api/memory'
            report['checks']['private_memory_disabled_by_default'] = request(memory,tokens['alice'])[0] == 403
            stop(process)
            host = json.loads((root/'host.json').read_text(encoding='utf-8'))
            host['private_memory'] = {'curate':True, 'recall':{'max_items':8,'max_bytes':16384,'include_preferences':True}}
            save('host.json',host)
            process = start()
            snapshots = {name:request(memory,token) for name,token in tokens.items()}
            report['checks']['explicit_host_private_memory_binds_own_actor'] = all(
                value[0] == 200 and value[1]['scope']['owner'] == name for name,value in snapshots.items())
            report['checks']['private_memory_scope_query_cannot_choose_other_owner'] = request(memory+'?owner=bob',tokens['alice'])[0] == 400
            report['checks']['shared_session_private_extraction_stays_denied'] = request(memory+'/extract',tokens['alice'],{})[0] == 403
        if os.name != 'nt':
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=15)
            report['checks']['sigterm_drains_and_exits_successfully'] = process.returncode == 0
        else:
            process.terminate()
            process.wait(timeout=15)
            report['sigterm_windows'] = 'not tested: explicit termination of owned idle process; no active model/tool work'
        report['checks']['zero_model_requests'] = not count
        report['status'] = 'passed' if all(report['checks'].values()) else 'failed'
    except Exception as error:
        report.update(status='failed', error=str(error))
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=15)
        provider.shutdown()
        provider.server_close()
        thread.join(timeout=5)
        log.close()
        report['observed_provider_requests'] = len(count)
        save('report.json',report)
    print(json.dumps(report))
    raise SystemExit(0 if report['status']=='passed' else 1)


if __name__ == '__main__':
    main()
