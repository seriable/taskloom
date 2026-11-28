"""Export an exact Git commit and build an isolated, fingerprinted old binary.

No checkout edits, lockfile updates, schema operation, service or model request.
Use an ignored output directory and the same explicit compiler as the new build.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import tarfile


def save(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--repository',type=Path,required=True)
    parser.add_argument('--ref',default='0ae4bca')
    parser.add_argument('--expected-commit',default='0ae4bca0b5e85ca052f419a8fe0de0560c23fac3')
    parser.add_argument('--toolchain',default='1.95.0')
    parser.add_argument('--features',default='app,database')
    args=parser.parse_args();root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    repository=args.repository.resolve();features=sorted(set(args.features.split(',')))
    report={'version':1,'completed':False,'failures':[],'scope':__doc__,'features':features,'toolchain':args.toolchain,'platform':platform.platform(),'built_at_utc':datetime.now(timezone.utc).isoformat()}
    git=['git','-c','safe.directory='+str(repository),'-C',str(repository)]
    try:
        commit=subprocess.check_output([*git,'rev-parse','--verify','--end-of-options',args.ref+'^{commit}'],text=True,timeout=30).strip()
        if not re.fullmatch('[a-f0-9]{40}',args.expected_commit) or commit!=args.expected_commit:
            raise ValueError('actual source commit differs from explicit expected commit; build refused')
        if features!=['app','database']:
            raise ValueError('this rolling fixture builds only the explicit common app,database profile')
        report['source_commit']=commit
        report['source_tree']=subprocess.check_output([*git,'rev-parse',commit+'^{tree}'],text=True,timeout=30).strip()
        archive=root/'source.tar'
        with archive.open('xb') as output:
            subprocess.run([*git,'archive','--format=tar',commit],stdout=output,stderr=subprocess.PIPE,check=True,timeout=60)
        report['archive_sha256']=digest(archive)
        source=root/'source';source.mkdir()
        with tarfile.open(archive,'r:') as package:
            members=package.getmembers()
            for member in members:
                path=PurePosixPath(member.name)
                if path.is_absolute() or '..' in path.parts or any(':' in part or '\\' in part for part in path.parts) or not (member.isfile() or member.isdir()):
                    raise ValueError('archive contains a nonregular or escaping member; extraction refused')
                destination=source.joinpath(*path.parts).resolve()
                if destination!=source and source not in destination.parents:
                    raise ValueError('archive destination escapes the explicitly owned source directory')
            for member in members:
                destination=source.joinpath(*PurePosixPath(member.name).parts)
                if member.isdir():destination.mkdir(parents=True,exist_ok=True)
                else:
                    destination.parent.mkdir(parents=True,exist_ok=True)
                    with package.extractfile(member) as incoming,destination.open('xb') as outgoing:shutil.copyfileobj(incoming,outgoing)
                    destination.chmod(member.mode & 0o777)
        lock=source/'Cargo.lock';report['cargo_lock_sha256']=digest(lock)
        report['rustc']=subprocess.check_output(['rustc','+'+args.toolchain,'--version','--verbose'],text=True,timeout=30).strip()
        report['cargo']=subprocess.check_output(['cargo','+'+args.toolchain,'--version'],text=True,timeout=30).strip()
        target=root/'cargo-target';environment=dict(os.environ,CARGO_TARGET_DIR=str(target))
        with (root/'build.log').open('xb') as output:
            result=subprocess.run(['cargo','+'+args.toolchain,'build','--locked','--features',','.join(features),'--bin','taskloom'],cwd=source,env=environment,stdout=output,stderr=output,timeout=1800)
        if result.returncode:raise RuntimeError('archived source build failed; original log retained')
        if digest(lock)!=report['cargo_lock_sha256']:raise ValueError('locked source Cargo.lock changed; refuse binary receipt')
        built=target/'debug'/('taskloom.exe' if os.name=='nt' else 'taskloom');binary=root/('taskloom-old.exe' if os.name=='nt' else 'taskloom-old')
        with built.open('rb') as incoming,binary.open('xb') as outgoing:shutil.copyfileobj(incoming,outgoing)
        binary.chmod(built.stat().st_mode)
        report['binary']={'path':str(binary),'sha256':digest(binary),'bytes':binary.stat().st_size,'version':subprocess.check_output([str(binary),'--version'],text=True,timeout=15).strip()}
        schema=re.search(r'pub const DATABASE_SCHEMA_VERSION: i64 = (\d+);',(source/'src/runtime/database.rs').read_text())
        if schema is None:raise ValueError('archived schema constant not found')
        report['schema_version']=int(schema.group(1));report['build_log_sha256']=digest(root/'build.log');report['completed']=True
    except Exception as error:report['failures'].append(str(error))
    report['status']='passed' if report['completed'] and not report['failures'] else 'failed';save(root/'report.json',report)
    print(json.dumps(report));raise SystemExit(0 if report['status']=='passed' else 1)


if __name__=='__main__':main()
