"""Create/remove an explicitly owned loopback PostgreSQL acceptance container.

No published binaries or account service. The pinned official image must already
exist locally; this helper never changes daemon/proxy settings or existing
containers. credentials.json is private local test data, never a source artifact.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid

IMAGE='postgres@sha256:aa6cb33cf4c19c765cefcf48d96176258800870a9a1e930ad126200f9d825139'
LABEL='com.taskloom.database-acceptance'


def docker(*args):
    return subprocess.check_output(['docker',*args],text=True,stderr=subprocess.PIPE,timeout=40).strip()


def save(path,value):
    with path.open('x',encoding='utf-8') as stream:json.dump(value,stream,indent=2);stream.write('\n')


def create(root):
    root=root.resolve();root.mkdir(parents=True,exist_ok=False)
    if docker('info','--format','{{.OSType}}')!='linux':raise ValueError('a Docker Linux engine is required')
    image=json.loads(docker('image','inspect',IMAGE,'--format','{{json .}}'))
    if IMAGE not in image['RepoDigests']:raise ValueError('local image does not match the explicit official digest')
    nonce=uuid.uuid4().hex;password=uuid.uuid4().hex+uuid.uuid4().hex
    environment=dict(os.environ,POSTGRES_PASSWORD=password)
    # The secret travels in the owned child's environment, never its argv/log.
    result=subprocess.run(['docker','run','--pull','never','--detach','--name','taskloom-db-'+nonce[:12],
        '--label',LABEL+'='+nonce,'--publish','127.0.0.1::5432','--env','POSTGRES_PASSWORD',
        '--env','POSTGRES_USER=taskloom_accept','--env','POSTGRES_DB=taskloom_accept',IMAGE],
        env=environment,text=True,capture_output=True,timeout=40)
    if result.returncode:raise RuntimeError('owned PostgreSQL container creation failed; Docker returned code '+str(result.returncode))
    identity=result.stdout.strip()
    if not re.fullmatch('[a-f0-9]{64}',identity):raise ValueError('unexpected created container identity')
    # Persist identity before readiness so failed startup is still removable.
    save(root/'ownership.json',{'container_id':identity,'nonce':nonce,'image_id':image['Id']})
    bindings=json.loads(docker('inspect',identity,'--format','{{json .NetworkSettings.Ports}}'))['5432/tcp']
    if len(bindings)!=1 or bindings[0]['HostIp']!='127.0.0.1':raise ValueError('acceptance database must bind loopback only')
    port=int(bindings[0]['HostPort'])
    save(root/'credentials.json',{'TASKLOOM_TEST_POSTGRES_URL':f'postgres://taskloom_accept:{password}@127.0.0.1:{port}/taskloom_accept?sslmode=disable'})
    for _ in range(100):
        # Official images briefly accept Unix-socket connections in their
        # init-only server before shutting it down. Wait for the final TCP
        # listener, not that transient socket readiness.
        ready=subprocess.run(['docker','exec',identity,'pg_isready','-h','127.0.0.1','-U','taskloom_accept','-d','taskloom_accept'],capture_output=True,timeout=5)
        if ready.returncode==0:break
        time.sleep(0.1)
    else:raise RuntimeError('owned PostgreSQL readiness deadline reached; ownership file retained')
    settings={name:docker('exec',identity,'psql','-U','taskloom_accept','-d','taskloom_accept','-Atc','SHOW '+name)
              for name in ['server_version','fsync','full_page_writes','default_transaction_isolation']}
    public={'version':1,'source':'explicit cached official PostgreSQL digest; no new image pull','image':IMAGE,'image_id':image['Id'],
        'container_id':identity,'loopback_port':port,'credential_environment':'TASKLOOM_TEST_POSTGRES_URL',
        'credential_values':'private credentials.json; never publish','settings':settings,'scope':'owned local disposable database acceptance, not application deployment or load evidence'}
    save(root/'public.json',public);print(json.dumps(public))


def remove(root):
    ownership=json.loads((root/'ownership.json').read_text())
    identity=ownership['container_id'];nonce=ownership['nonce']
    if not re.fullmatch('[a-f0-9]{64}',identity) or not re.fullmatch('[a-f0-9]{32}',nonce):raise ValueError('invalid owned container identity')
    state=json.loads(docker('inspect',identity,'--format','{{json .}}'))
    if state['Config']['Labels'].get(LABEL)!=nonce or state['Image']!=ownership['image_id']:
        raise ValueError('container ownership/image changed; refuse removal')
    # Remove only the exact created container and its anonymous test data volume.
    docker('rm','--force','--volumes',identity)
    print(json.dumps({'removed_owned_container':identity}))


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('action',choices=['create','remove']);parser.add_argument('directory',type=Path)
    args=parser.parse_args()
    if args.action=='create':create(args.directory)
    else:remove(args.directory.resolve())


if __name__=='__main__':main()
