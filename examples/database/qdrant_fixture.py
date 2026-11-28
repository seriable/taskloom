"""Owned loopback Qdrant acceptance fixture; no cloud service or paid model.

Uses an explicitly cached, pinned image of upstream qdrant/qdrant v1.20.2,
downloaded through the named third-party DaoCloud registry mirror. This is not
proof of a direct Docker Hub download. Never modifies Docker daemon settings.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
from urllib.error import URLError
from urllib.request import ProxyHandler, Request, build_opener
import uuid


IMAGE = 'docker.m.daocloud.io/qdrant/qdrant@sha256:d277592721d2e3b73f9b797b53cf05e6a5bb2134f6d13fdbda4ca51782afbcac'
LABEL = 'com.taskloom.qdrant-acceptance'
KEY_ENV = 'TASKLOOM_QDRANT_FIXTURE_KEY'
VERSION = '1.20.2'


def save(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def digest(path):
    return {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def docker(*parts):
    result = subprocess.run(['docker', *parts], capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError('owned Qdrant Docker operation failed: '+str(parts[0]))
    return result.stdout.decode('utf-8').strip()


def owned(root):
    record = json.loads((root/'ownership.json').read_text())
    cid = record['container_id']
    if not re.fullmatch('[a-f0-9]{64}', cid) or not re.fullmatch('[a-f0-9]{32}', record['nonce']):
        raise ValueError('invalid Qdrant ownership identity')
    state = json.loads(docker('inspect', cid, '--format', '{{json .}}'))
    if state['Config']['Labels'].get(LABEL) != record['nonce'] or state['Image'] != record['image_id']:
        raise ValueError('Qdrant ownership/image changed; refuse mutation')
    return cid


def http(endpoint, key, path, body=None, method=None):
    data = None if body is None else json.dumps(body).encode()
    request = Request(endpoint.rstrip('/')+path, data=data,
                      headers={'api-key': key, 'Content-Type': 'application/json'}, method=method)
    with build_opener(ProxyHandler({})).open(request, timeout=5) as response:
        raw = response.read(1024*1024)
        return None if not raw else json.loads(raw)


def create(root):
    root.mkdir(parents=True, exist_ok=False)
    if docker('info', '--format', '{{.OSType}}') != 'linux':
        raise ValueError('Docker Linux engine required')
    image = json.loads(docker('image', 'inspect', IMAGE, '--format', '{{json .}}'))
    if IMAGE not in image['RepoDigests']:
        raise ValueError('cached image does not match the explicit mirror digest; no implicit pull')
    nonce = uuid.uuid4().hex
    key = uuid.uuid4().hex+uuid.uuid4().hex
    environment = dict(os.environ, QDRANT__SERVICE__API_KEY=key)
    # Secret value travels only through the owned child environment.
    child = subprocess.run(['docker', 'run', '--pull', 'never', '--detach', '--name', 'taskloom-qdrant-'+nonce[:12],
                            '--label', LABEL+'='+nonce, '--publish', '127.0.0.1::6333', '--volume', '/qdrant/storage',
                            '--env', 'QDRANT__SERVICE__API_KEY', '--env', 'QDRANT__TELEMETRY_DISABLED=true', IMAGE],
                           env=environment, capture_output=True, timeout=60)
    if child.returncode:
        raise RuntimeError('owned Qdrant container start failed')
    cid = child.stdout.decode().strip()
    if not re.fullmatch('[a-f0-9]{64}', cid):
        raise ValueError('unexpected created Qdrant container identity')
    save(root/'ownership.json', {'container_id': cid, 'nonce': nonce, 'image_id': image['Id']})
    save(root/'credentials.json', {KEY_ENV: key})
    ports = json.loads(docker('inspect', cid, '--format', '{{json .NetworkSettings.Ports}}'))['6333/tcp']
    if len(ports) != 1 or ports[0]['HostIp'] != '127.0.0.1':
        raise ValueError('Qdrant fixture must bind loopback only')
    endpoint = 'http://127.0.0.1:'+ports[0]['HostPort']+'/'
    for _ in range(100):
        try:
            info = http(endpoint, key, '/')
            break
        except (URLError, OSError):
            time.sleep(.1)
    else:
        raise RuntimeError('Qdrant readiness deadline; owned fixture identity retained')
    if info['version'] != VERSION:
        raise ValueError('running Qdrant version differs from the verified upstream release tag')
    collection = 'taskloom_owned_'+nonce
    owned(root)
    result = http(endpoint, key, '/collections/'+collection,
                  {'vectors': {'size': 2, 'distance': 'Cosine'}}, 'PUT')
    if result['status'] != 'ok' or result['result'] is not True:
        raise RuntimeError('owned Qdrant collection creation did not acknowledge completion')
    config = {'endpoint': endpoint, 'collection': collection, 'namespace': 'taskloom-fixture-'+nonce,
              'api_key_env': KEY_ENV, 'timeout_ms': 5000, 'max_response_bytes': 1024*1024,
              'max_request_bytes': 1024*1024, 'batch_points': 1}
    save(root/'host-config.json', config)
    public = {'version': 1, 'image': IMAGE, 'image_id': image['Id'], 'container_id': cid,
              'registry_source': 'explicit third-party DaoCloud forwarding of qdrant/qdrant; direct Docker Hub access timed out',
              'upstream_release': 'https://github.com/qdrant/qdrant/releases/tag/v'+VERSION,
              'running_server': info, 'endpoint': endpoint, 'collection': collection,
              'credential_environment': KEY_ENV, 'credential_values': 'private credentials.json; never publish',
              'scope': 'owned single-host Qdrant REST acceptance; no cloud/production SLO or semantic-quality claim'}
    save(root/'public.json', public)
    print(json.dumps(public))


def accept(fixture, root, executable):
    root.mkdir(parents=True, exist_ok=False)
    owned(fixture)
    public = json.loads((fixture/'public.json').read_text())
    config = json.loads((fixture/'host-config.json').read_text())
    key = json.loads((fixture/'credentials.json').read_text())[KEY_ENV]
    copied = root/('agent_qdrant'+executable.suffix)
    with executable.open('rb') as incoming, copied.open('xb') as outgoing:
        shutil.copyfileobj(incoming, outgoing)
    copied.chmod(executable.stat().st_mode)
    report = {'version': 1, 'completed': False, 'checks': {}, 'failures': [],
              'scope': __doc__, 'provenance': public, 'binary': digest(copied), 'remote_model_requests': 0}
    try:
        child = subprocess.run([str(copied), str(fixture/'host-config.json'), str(root/'example')],
                               env=dict(os.environ, **{KEY_ENV: key}), capture_output=True, timeout=120)
        with (root/'example.log').open('x', encoding='utf-8') as stream:
            stream.write((child.stdout+child.stderr).decode('utf-8', errors='replace').replace(key, '[private fixture credential]'))
        if child.returncode:
            raise RuntimeError('real Qdrant example failed; sanitized local log retained')
        result = json.loads((root/'example/report.json').read_text())
        report['checks'].update(result['result']['checks'])
        owned(fixture)
        total = http(config['endpoint'], key, '/collections/'+config['collection']+'/points/count', {'exact': True})
        report['checks']['real_service_has_exactly_three_explicitly_published_points'] = total['result']['count'] == 3
        counts = {}
        for name, namespace in [('main', config['namespace']), ('foreign', config['namespace']+'::foreign')]:
            value = http(config['endpoint'], key, '/collections/'+config['collection']+'/points/count',
                         {'exact': True, 'filter': {'must': [{'key': 'taskloom_scope', 'match': {'value': namespace}}]}})
            counts[name] = value['result']['count']
        report['checks']['real_service_scope_filters_separate_two_and_one_points'] = counts == {'main': 2, 'foreign': 1}
        report['real_point_counts'] = counts
        report['completed'] = True
    except Exception as error:
        report['failures'].append(str(error).replace(key, '[private fixture credential]'))
    report['failures'].extend('check failed: '+name for name, passed in report['checks'].items() if not passed)
    report['status'] = 'passed' if report['completed'] and not report['failures'] else 'failed'
    source = Path(__file__).resolve().parents[2]
    report['sources'] = {name: digest(source/name) for name in ['Cargo.toml', 'src/runtime/rag/qdrant.rs', 'examples/agent_qdrant.rs', 'examples/database/qdrant_fixture.py']}
    report['logs'] = {path.name: digest(path) for path in root.glob('*.log')}
    save(root/'report.json', report)
    print(json.dumps(report))
    raise SystemExit(0 if report['status'] == 'passed' else 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['create', 'remove', 'accept'])
    parser.add_argument('fixture', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--example', type=Path)
    args = parser.parse_args()
    root = args.fixture.resolve()
    if args.action == 'create':
        create(root)
    elif args.action == 'remove':
        cid = owned(root)
        docker('rm', '--force', '--volumes', cid)
        print(json.dumps({'removed_owned_container': cid}))
    else:
        if args.out is None or args.example is None:
            parser.error('accept requires --out and --example')
        accept(root, args.out.resolve(), args.example.resolve())


if __name__ == '__main__':
    main()
