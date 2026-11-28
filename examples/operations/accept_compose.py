"""Owned Compose SQL/API/worker plumbing smoke, no queued task/model request.

Requires a previously built reviewed image and cached PostgreSQL16.15 image.
Uses disposable local credentials, loopback publishing and a unique project;
removes only that owned project's containers/volumes, never user deployments.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--image',required=True)
    parser.add_argument('--docker',default='docker')
    args=parser.parse_args()
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    checkout=Path(__file__).resolve().parents[2]
    nonce=uuid.uuid4().hex
    project='taskloom-deploy-'+nonce[:12]
    secret_directory=Path(tempfile.gettempdir()).resolve()/('taskloom-deploy-'+nonce)
    environment=dict(os.environ,TASKLOOM_IMAGE=args.image,TASKLOOM_SECRETS_DIR=str(secret_directory),TASKLOOM_COMPOSE_SMOKE_KEY='disposable-no-model-task-'+nonce)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));environment['TASKLOOM_API_PORT']=str(sock.getsockname()[1])
    compose=[args.docker,'compose','--project-name',project,'--file',str(checkout/'deploy/compose.yaml')]
    def run(name,arguments):
        value=subprocess.run(arguments,env=environment,capture_output=True,text=True,encoding='utf-8',timeout=120)
        (root/(name+'.log')).write_text(value.stdout+value.stderr,encoding='utf-8')
        if value.returncode:raise RuntimeError('owned command failed: '+name)
        return value.stdout
    report={'scope':'Actual owned Compose PostgreSQL/API/idle-worker plumbing; configured native provider has only disposable unused key, no task/model effectiveness claim.',
            'compose_project':project,'image':args.image,'model_tasks_submitted':0,'checks':{}}
    started=False
    try:
        run('private-secret-preparation',[os.sys.executable,str(checkout/'examples/operations/prepare_compose_secrets.py'),str(secret_directory),'--model-key-env','TASKLOOM_COMPOSE_SMOKE_KEY'])
        inspected=json.loads(run('image-inspect',[args.docker,'image','inspect',args.image]))[0]
        report['image_id']=inspected['Id']
        report['checks']['image_non_root_and_sigterm']=inspected['Config']['User']=='10001:10001' and inspected['Config']['StopSignal']=='SIGTERM'
        started=True
        run('database-start',compose+['up','--detach','--no-build','--wait','--wait-timeout','60','database'])
        run('explicit-migration',compose+['run','--rm','--no-deps','migrate'])
        run('explicit-quota',compose+['run','--rm','--no-deps','admin','taskloom-admin','quota-set','team','main','/etc/taskloom/quota.json','0','deployment-smoke','explicit fixture cap'])
        run('explicit-queue',compose+['run','--rm','--no-deps','configure-queue'])
        run('api-workers-start',compose+['up','--detach','--no-build','--scale','worker=2','api','worker'])
        for _ in range(100):
            try:
                with urllib.request.urlopen('http://127.0.0.1:'+environment['TASKLOOM_API_PORT']+'/health/ready',timeout=3) as response:
                    if response.status==200 and json.load(response)=={'ready':True}:break
            except OSError:pass
            time.sleep(.1)
        else:raise RuntimeError('owned API did not become ready')
        report['checks']['actual_postgres_api_ready']=True
        run('cold-check-without-model-key',compose+['exec','-T','api','sh','-ec','unset TASKLOOM_MODEL_KEY TASKLOOM_MODEL_KEY_FILE; exec /usr/local/bin/taskloom-entrypoint taskloom service-check /etc/taskloom/host.json'])
        report['checks']['cold_probe_needs_no_model_key']=True
        run('health-cli',compose+['exec','-T','api','taskloom','health-check','http://127.0.0.1:8080/health/ready'])
        containers=run('container-list',compose+['ps','--all','--quiet']).splitlines()
        states=[]
        for identity in containers:
            state=json.loads(run('inspect-'+identity[:12],[args.docker,'inspect',identity]))[0]
            assert state['Config']['Labels']['com.docker.compose.project']==project
            states.append(state)
        applications=[state for state in states if state['Config']['Labels']['com.docker.compose.service'] in ['api','worker']]
        report['checks']['actual_two_independent_workers']=sum(state['Config']['Labels']['com.docker.compose.service']=='worker' for state in applications)==2
        report['checks']['application_containers_restricted']=all(state['HostConfig']['ReadonlyRootfs'] and 'ALL' in state['HostConfig']['CapDrop'] and state['Config']['User']=='10001:10001' for state in applications)
        report['checks']['no_docker_socket_mount']=all(not any(mount['Destination'].endswith('docker.sock') for mount in state['Mounts']) for state in applications)
        run('sigterm-stop',compose+['stop','--timeout','45','api','worker'])
        exits=[]
        for state in applications:
            stopped=json.loads(run('stopped-'+state['Id'][:12],[args.docker,'inspect',state['Id']]))[0]
            exits.append(stopped['State']['ExitCode'])
        report['checks']['api_and_idle_workers_sigterm_exit_zero']=all(code==0 for code in exits)
        report['status']='passed' if all(report['checks'].values()) else 'failed'
    except Exception as error:report.update(status='failed',error=str(error))
    finally:
        if started:
            try:run('owned-project-cleanup',compose+['down','--volumes','--remove-orphans','--timeout','45']);report['owned_project_removed']=True
            except Exception as error:report['cleanup_error']=str(error)
        # Only the three exact create-only files in this nonce-owned temp path.
        if secret_directory.parent==Path(tempfile.gettempdir()).resolve() and secret_directory.name=='taskloom-deploy-'+nonce:
            for name in ['model-key','database-url','postgres-password']:
                path=secret_directory/name
                if path.is_file():path.unlink()
            if secret_directory.exists():secret_directory.rmdir()
        report['compose_source_sha256']=hashlib.sha256((checkout/'deploy/compose.yaml').read_bytes()).hexdigest()
        (root/'report.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(report))
    raise SystemExit(0 if report['status']=='passed' else 1)


if __name__=='__main__':main()
