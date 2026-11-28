"""Render deployment assets and verify fail-closed chart contracts locally.

Requires Helm 3, Docker Compose v2 (config only), and PyYAML. Never creates a
container/cluster resource, reads secret files, or calls a model.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import yaml


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--helm', default='helm')
    parser.add_argument('--docker', default='docker')
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    checkout = Path(__file__).resolve().parents[2]
    chart = checkout/'deploy/helm/taskloom'
    checks = {}
    def command(name, arguments, successful=True, env=None):
        run = subprocess.run(list(map(str,arguments)),capture_output=True,text=True,encoding='utf-8',env=env,timeout=30)
        (root/(name+'.log')).write_text(run.stdout+run.stderr,encoding='utf-8')
        checks[name] = (run.returncode == 0) == successful
        if not checks[name]:
            raise RuntimeError('unexpected deployment check result: '+name)
        return run.stdout
    command('helm-lint',[args.helm,'lint',chart])
    cases = [
        ('default',[],True),
        ('cpu-hpa',['--set','autoscaling.worker.enabled=true','--set','autoscaling.api.enabled=true'],True),
        ('unsafe-hpa',['--set','autoscaling.worker.enabled=true','--set','autoscaling.worker.maxReplicas=5'],False),
        ('unsafe-migration',['--set','migration.enabled=true','--set','migration.jobId=smoke'],False),
        ('quiescent-migration',['--set','migration.enabled=true','--set','migration.jobId=smoke','--set','maintenance.enabled=true','--set','api.replicas=0','--set','worker.replicas=0','--set','migration.configureQueue=true'],True),
        ('maintenance-hpa',['--set','maintenance.enabled=true','--set','autoscaling.api.enabled=true'],False),
        ('operator-shell-expansion',['--set','migration.enabled=true','--set','migration.jobId=smoke','--set','maintenance.enabled=true','--set','api.replicas=0','--set','worker.replicas=0','--set-string','migration.operator=$(not-a-command)'],False),
    ]
    for name, flags, success in cases:
        output = command('helm-'+name,[args.helm,'template','local',chart,*flags],success)
        if not success:
            continue
        documents = list(yaml.safe_load_all(output))
        if any(doc['kind'] in ['Role','ClusterRole','RoleBinding','ClusterRoleBinding','Secret'] for doc in documents):
            raise RuntimeError('runtime chart must not grant Kubernetes privileges or render credentials')
        for document in documents:
            if document['kind'] == 'ConfigMap':
                for value in document['data'].values():
                    json.loads(value)
            if document['kind'] in ['Deployment','Job']:
                pod = document['spec']['template']['spec']
                assert pod['automountServiceAccountToken'] is False
                assert pod['securityContext']['runAsNonRoot'] is True
                for container in pod['containers']:
                    assert container['securityContext']['readOnlyRootFilesystem']
                    assert not container['securityContext']['allowPrivilegeEscalation']
                    assert container['securityContext']['capabilities']['drop'] == ['ALL']
                    assert all('secretKeyRef' in entry['valueFrom'] for entry in container['env'])
                if document['kind'] == 'Deployment':
                    assert document['spec']['strategy']['type'] == 'Recreate'
                    if name == 'quiescent-migration':
                        assert document['spec']['replicas'] == 0
                    if name == 'cpu-hpa':
                        assert 'replicas' not in document['spec']
            if document['kind'] == 'HorizontalPodAutoscaler':
                assert document['spec']['metrics'][0]['type'] == 'Resource'
                assert document['spec']['metrics'][0]['resource']['name'] == 'cpu'
        checks[name+'-parsed-security-contracts'] = True
    environment = dict(os.environ, TASKLOOM_SECRETS_DIR=str(root/'unread-secret-placeholders'))
    output = command('compose-config',[args.docker,'compose','--file',checkout/'deploy/compose.yaml','config','--format','json','--no-env-resolution'],env=environment)
    compose = json.loads(output)
    assert not compose['services']['api'].get('privileged',False)
    assert compose['services']['api']['read_only']
    assert compose['services']['worker']['read_only']
    assert 'TASKLOOM_MODEL_KEY' not in compose['services']['api']['environment']
    assert compose['services']['api']['environment']['TASKLOOM_MODEL_KEY_FILE'] == '/run/secrets/model-key'
    checks['compose-uses-file-secrets-and-separated-worker'] = True
    files = [p for p in (checkout/'deploy').rglob('*') if p.is_file()]
    report = {'kind':'Actual Helm lint/render, parsed manifests and Compose config; no cluster/container/model execution',
              'checks':checks,'source_sha256':{str(path.relative_to(checkout)).replace('\\','/'):hashlib.sha256(path.read_bytes()).hexdigest() for path in files},
              'status':'passed'}
    (root/'report.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    print(json.dumps({'checks':len(checks),'status':report['status']}))


if __name__ == '__main__':
    main()
