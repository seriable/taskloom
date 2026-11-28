"""Actual local Prometheus scrape, authorization and alert-delivery drill.

Creates an owned SQLite host and a counting provider which always refuses.
No task is submitted. Uses a pre-cached, explicitly selected Prometheus image.
Only the QueueDisabled rule's hold time is shortened for this bounded drill;
the unmodified production rule durations require separate promtool tests.
The local receiver implements Alertmanager's receive protocol, not its routing,
deduplication or persistence. No external alert destination is contacted.
"""
import argparse
import hashlib
import hmac
import http.server
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--admin', type=Path, required=True)
    parser.add_argument('--image', required=True, help='pre-cached immutable image@sha256 digest')
    parser.add_argument('--docker-host', help='explicit Docker engine endpoint')
    parser.add_argument('--docker-hostname', default='host.docker.internal')
    args = parser.parse_args()
    if not re.fullmatch(r'[^\s]+@sha256:[a-f0-9]{64}', args.image):
        parser.error('immutable digest image required')
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    mount = root/'monitor'
    mount.mkdir()
    nonce = uuid.uuid4().hex
    name = 'taskloom-monitor-'+nonce
    docker = ['docker'] + (['--host', args.docker_host] if args.docker_host else [])
    exe, admin = args.taskloom.resolve(), args.admin.resolve()
    environment = dict(os.environ, TASKLOOM_MONITOR_DATABASE='sqlite://'+(root/'service.sqlite').as_posix()+'?mode=rwc',
                       TASKLOOM_MONITOR_KEY='disposable-unused-'+nonce)
    environment['TASKLOOM_DATABASE_URL'] = environment['TASKLOOM_MONITOR_DATABASE']
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    counters = {'provider_requests': 0, 'receiver_posts': 0}
    alerts = []
    receiver_key = os.urandom(32).hex()
    lock = threading.Lock()
    host = None
    servers = []
    threads = []
    started = False
    image_id = None
    report = {'version': 1, 'status': 'failed', 'checks': {}, 'failures': [],
              'scope': __doc__, 'taskloom_sha256': sha(exe), 'admin_sha256': sha(admin),
              'image': args.image, 'production_slo_accepted': False, 'cloud_model_requests': 0}

    def save(path, value):
        with path.open('x', encoding='utf-8') as stream:
            json.dump(value, stream, indent=2, ensure_ascii=False)
            stream.write('\n')

    def run(parts, env=None, timeout=40):
        result = subprocess.run(list(map(str, parts)), env=env or environment, capture_output=True, timeout=timeout)
        if result.returncode:
            # Never echo full arguments, stdout or stderr: token paths and DB
            # URLs are intentionally confined to this private fixture directory.
            raise RuntimeError('owned command failed: '+Path(str(parts[0])).name+' (exit '+str(result.returncode)+')')
        return result.stdout

    def cli(*parts):
        return json.loads(run([exe, '--config', root/'project.json', *parts]))

    def get(url, token=None):
        request = urllib.request.Request(url, headers={} if token is None else {'Authorization': 'Bearer '+token})
        try:
            with opener.open(request, timeout=3) as response:
                raw = response.read(2*1024*1024+1)
                if len(raw) > 2*1024*1024:
                    raise ValueError('bounded drill response exceeded')
                return response.status, raw
        except urllib.error.HTTPError as error:
            return error.code, error.read(4096)

    def wait(check, label, seconds=40):
        start = time.monotonic()
        while time.monotonic()-start < seconds:
            if host is not None and host.poll() is not None:
                raise RuntimeError('owned host exited during '+label)
            try:
                if check():
                    return round((time.monotonic()-start)*1000)
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(.25)
        raise RuntimeError('deadline: '+label)

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def do_POST(self):
            with lock:
                counters['provider_requests'] += 1
            self.send_response(503)
            self.end_headers()

    class Receiver(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def do_POST(self):
            if not hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer '+receiver_key):
                self.send_error(401)
                return
            length = int(self.headers.get('Content-Length', '-1'))
            if self.path != '/api/v2/alerts' or not 0 <= length <= 65536:
                self.send_error(400)
                return
            try:
                values = json.loads(self.rfile.read(length))
                if not isinstance(values, list) or len(values) > 32:
                    raise ValueError('invalid alert batch')
                for value in values:
                    expected = {'alertname': 'TaskloomQueueDisabled', 'job': 'authorized',
                                'instance': target, 'severity': 'warning', 'drill_nonce': nonce}
                    if value['labels'] != expected:
                        raise ValueError('unexpected alert')
                with lock:
                    counters['receiver_posts'] += 1
                    alerts.extend(values)
                self.send_response(200)
                self.end_headers()
            except (ValueError, KeyError, TypeError):
                self.send_error(400)

    def serve(address, handler):
        server = http.server.ThreadingHTTPServer(address, handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        servers.append(server)
        threads.append(thread)
        thread.start()
        return server

    def free_port():
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            return listener.getsockname()[1]

    log = (root/'host.log').open('xb')
    try:
        identity = json.loads(run(docker+['image', 'inspect', args.image, '--format', '{{json .}}']))
        image_id = identity['Id']
        report['image_id'] = image_id
        provider = serve(('127.0.0.1', 0), Provider)
        # Docker Desktop forwards host.docker.internal to these host listeners.
        # The explicit ephemeral listener is only a disposable alert receiver.
        receiver = serve(('0.0.0.0', 0), Receiver)
        port, prom_port = free_port(), free_port()
        run([exe, 'init', root/'generated.json'])
        config = json.loads((root/'generated.json').read_text(encoding='utf-8'))
        config.update(state_dir=str(root/'state'), knowledge=False, model='not-called',
                      provider={'type': 'openai', 'base_url': 'http://127.0.0.1:'+str(provider.server_port)+'/v1',
                                'key_env': 'TASKLOOM_MONITOR_KEY', 'namespace': 'monitoring-fixture'},
                      database={'url_env': 'TASKLOOM_MONITOR_DATABASE', 'tenant': 'monitor', 'project': 'main'})
        save(root/'project.json', config)
        save(root/'host.json', {'listen': '127.0.0.1:'+str(port), 'database_url_env': 'TASKLOOM_MONITOR_DATABASE',
                                'projects': [{'tenant': 'monitor', 'project': 'main', 'config': 'project.json'}]})
        save(root/'limits.json', {'max_open_jobs': 4, 'max_running': 2})
        save(root/'disabled.json', None)
        cli('database-migrate')
        cli('queue-configure', root/'limits.json', 0, 'PRIVATE_MONITOR_OPERATOR', 'PRIVATE_MONITOR_NOTE')
        save(root/'reader.json', {'id': 'monitor-reader', 'enabled': True,
                                  'memberships': [{'tenant': 'monitor', 'project': 'main', 'role': 'reader'}]})
        run([admin, 'put-user', root/'reader.json', '0', 'monitor-fixture'])
        run([admin, 'issue', 'monitor-reader', mount/'bearer.private', 'monitor-fixture'])
        token = (mount/'bearer.private').read_text(encoding='utf-8').strip()
        metric_path = '/tenants/monitor/projects/main/api/operations/metrics'
        endpoint = 'http://127.0.0.1:'+str(port)
        host = subprocess.Popen([str(exe), 'service-serve', str(root/'host.json')], env=environment, stdout=log, stderr=log,
                                creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        report['startup_ms'] = wait(lambda: get(endpoint+'/health/ready')[0] == 200, 'host ready')
        report['checks']['unauthenticated_metrics_refused'] = get(endpoint+metric_path)[0] == 401
        report['checks']['reader_metrics_authorized'] = get(endpoint+metric_path, token)[0] == 200
        report['checks']['cross_project_metrics_refused'] = get(endpoint+metric_path.replace('/main/', '/other/'), token)[0] in {403, 404}
        source = Path(__file__).resolve().parents[2]/'deploy/monitoring/alerts.yaml'
        # Parse no external YAML: the repository rule has exactly this duration.
        raw = source.read_text(encoding='utf-8')
        marker = '      - alert: TaskloomQueueDisabled\n'
        selected = raw.split(marker, 1)[1].split('      - alert:', 1)[0]
        if selected.count('for: 5m') != 1:
            raise ValueError('source rule changed; review bounded drill transform')
        if selected.count('labels: {severity: warning}') != 1:
            raise ValueError('source rule labels changed; review drill binding')
        selected = selected.replace('for: 5m', 'for: 2s').replace('labels: {severity: warning}', 'labels: {severity: warning, drill_nonce: "'+nonce+'"}')
        (mount/'alerts.yaml').write_text('groups:\n  - name: bounded-drill\n    rules:\n'+marker+selected, encoding='utf-8')
        report['rule_source_sha256'] = sha(source)
        report['bounded_rule_sha256'] = sha(mount/'alerts.yaml')
        report['rule_transform'] = 'only QueueDisabled selected, for 5m -> 2s, independent drill_nonce label added; expression/severity/annotations preserved'
        target = args.docker_hostname+':'+str(port)
        with (mount/'alert-bearer.private').open('x', encoding='utf-8') as stream:
            stream.write(receiver_key)
        forged = urllib.request.Request('http://127.0.0.1:'+str(receiver.server_port)+'/api/v2/alerts',
                                        data=b'[]', headers={'Content-Type': 'application/json'})
        try:
            with opener.open(forged, timeout=3) as response:
                refused = response.status
        except urllib.error.HTTPError as error:
            refused = error.code
        report['checks']['alert_receiver_requires_independent_bearer'] = refused == 401 and not alerts
        # JSON is valid YAML. Secrets stay in the dedicated file, never config.
        save(mount/'prometheus.json', {'global': {'scrape_interval': '1s', 'evaluation_interval': '1s'},
            'rule_files': ['/fixture/alerts.yaml'], 'alerting': {'alertmanagers': [{'api_version': 'v2',
                'authorization': {'credentials_file': '/fixture/alert-bearer.private'},
                'static_configs': [{'targets': [args.docker_hostname+':'+str(receiver.server_port)]}]}]},
            'scrape_configs': [{'job_name': 'authorized', 'metrics_path': metric_path,
                'authorization': {'credentials_file': '/fixture/bearer.private'}, 'static_configs': [{'targets': [target]}]},
                {'job_name': 'unauthorized', 'metrics_path': metric_path, 'static_configs': [{'targets': [target]}]}]})
        started = True  # inspect even if docker lost the create/run acknowledgement
        run(docker+['run', '--detach', '--pull', 'never', '--name', name, '--label', 'com.taskloom.monitor='+nonce,
            '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--user', '65534:65534',
            '--pids-limit', '64', '--memory', '256m', '--cpus', '1', '--env', 'GOMAXPROCS=2',
            '--publish', '127.0.0.1:'+str(prom_port)+':9090', '--tmpfs', '/prometheus:rw,nosuid,nodev,size=64m,mode=1777',
            '--mount', 'type=bind,source='+str(mount)+',target=/fixture,readonly', args.image,
            '--config.file=/fixture/prometheus.json', '--storage.tsdb.path=/prometheus', '--storage.tsdb.retention.time=10m',
            '--web.listen-address=0.0.0.0:9090', '--rules.alert.resend-delay=1s'])
        prom = 'http://127.0.0.1:'+str(prom_port)

        def query(expression):
            status, raw = get(prom+'/api/v1/query?'+urllib.parse.urlencode({'query': expression}))
            if status != 200:
                return []
            return json.loads(raw)['data']['result']

        def scalar(expression, expected):
            result = query(expression)
            return len(result) == 1 and result[0]['value'][1] == expected

        report['scrape_ready_ms'] = wait(lambda: scalar('up{job="authorized"}', '1') and scalar('up{job="unauthorized"}', '0'), 'real authorized/unauthorized scrapes')
        report['checks']['prometheus_scrapes_actual_host_with_reader_token'] = scalar('taskloom_queue_enabled{job="authorized"}', '1')
        report['checks']['prometheus_unauthorized_scrape_up_zero'] = scalar('up{job="unauthorized"}', '0')
        cli('queue-configure', root/'disabled.json', 1, 'PRIVATE_MONITOR_OPERATOR', 'PRIVATE_MONITOR_NOTE')
        def delivered():
            with lock:
                received = bool(alerts)
            return received and scalar('ALERTS{alertname="TaskloomQueueDisabled",alertstate="firing"}', '1')
        report['alert_delivery_ms'] = wait(delivered, 'actual local alert receive and firing expression')
        report['checks']['disabled_queue_metric_and_readiness'] = scalar('taskloom_queue_enabled{job="authorized"}', '0') and get(endpoint+'/health/ready')[0] == 503
        report['checks']['actual_prometheus_alert_firing'] = scalar('ALERTS{alertname="TaskloomQueueDisabled",alertstate="firing"}', '1')
        report['checks']['receiver_received_real_prometheus_alert'] = bool(alerts)
        before_recovery = len(alerts)
        cli('queue-configure', root/'limits.json', 2, 'PRIVATE_MONITOR_OPERATOR', 'PRIVATE_MONITOR_NOTE')
        report['recovery_observed_ms'] = wait(lambda: scalar('taskloom_queue_enabled{job="authorized"}', '1') and not query('ALERTS{alertname="TaskloomQueueDisabled",alertstate="firing"}'), 'recovery expression')
        # Prometheus sends a resolved alert with endsAt in the past. Receiver
        # protocol here intentionally has no routing/dedup/persistence semantics.
        import datetime
        def resolved():
            now = datetime.datetime.now(datetime.timezone.utc)
            with lock:
                return any(a.get('endsAt') and datetime.datetime.fromisoformat(a['endsAt'].replace('Z', '+00:00')) <= now for a in alerts[before_recovery:])
        wait(resolved, 'resolved alert receive')
        report['checks']['receiver_received_resolution_and_host_ready'] = resolved() and get(endpoint+'/health/ready')[0] == 200
        status, raw = get(endpoint+metric_path, token)
        serialized_alerts = json.dumps(alerts)
        report['checks']['no_token_or_free_text_in_metrics_or_alerts'] = status == 200 and all(secret not in raw.decode()+serialized_alerts for secret in [token, receiver_key, 'PRIVATE_MONITOR_OPERATOR', 'PRIVATE_MONITOR_NOTE'])
        report['checks']['zero_provider_requests'] = counters['provider_requests'] == 0
        report['checks']['read_only_observations_never_submit_tasks'] = cli('queue')['jobs'] == []
        report['scrape_samples'] = {key: query(key) for key in ['up', 'taskloom_queue_enabled']}
        report['alert_labels'] = [a['labels'] for a in alerts]
    except Exception as error:
        report['failures'].append(str(error))
    finally:
        if started:
            try:
                listed = run(docker+['container', 'ls', '--all', '--filter', 'name=^/'+name+'$', '--format', '{{.ID}}'])
                if listed.strip():
                    state = json.loads(run(docker+['inspect', name, '--format', '{{json .}}']))
                    if state['Image'] != image_id or state['Config']['Labels'].get('com.taskloom.monitor') != nonce:
                        raise ValueError('container cleanup ownership mismatch')
                    try:
                        (root/'prometheus.log').write_bytes(run(docker+['logs', name]))
                    except Exception as error:
                        report['failures'].append('monitor log capture: '+str(error))
                    run(docker+['rm', '--force', state['Id']])
                remaining = run(docker+['container', 'ls', '--all', '--filter', 'name=^/'+name+'$', '--format', '{{.ID}}'])
                if remaining.strip():
                    raise ValueError('owned container still present')
                report['monitor_container_removed'] = True
            except Exception as error:
                report['failures'].append('monitor cleanup: '+str(error))
        if host is not None:
            try:
                if host.poll() is None:
                    host.send_signal(signal.SIGTERM) if os.name != 'nt' else host.terminate()
                    host.wait(timeout=30)
                report['host_stopped'] = host.poll() is not None
                report['host_shutdown'] = 'POSIX SIGTERM' if os.name != 'nt' else 'owned Windows idle process termination'
            except Exception as error:
                report['failures'].append('host cleanup: '+str(error))
                if host.poll() is None:
                    try:
                        host.kill()
                        host.wait(timeout=10)
                        report['host_forced_cleanup'] = True
                    except Exception as failure:
                        report['failures'].append('host forced cleanup: '+str(failure))
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
        log.close()
        report['counters'] = dict(counters)
        report['failures'].extend('check failed: '+key for key, passed in report['checks'].items() if not passed)
        report['status'] = 'passed' if report['checks'] and not report['failures'] else 'failed'
        save(root/'report.json', report)
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(0 if report['status'] == 'passed' else 1)


if __name__ == '__main__':
    main()
