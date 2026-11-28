"""Fingerprint an explicitly approved frozen Linux binary and its source tree.

This records a host build witness; the caller supplies the actual source root
used by Cargo. Separate hashes do not claim a hermetic reproducible build.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()


def source_files(root):
    paths=[root/'Cargo.toml',root/'Cargo.lock']+[p for p in sorted((root/'src').rglob('*')) if p.is_file()]
    if any(path.is_symlink() for path in paths):raise ValueError('source witness refuses symlinks')
    return {path.relative_to(root).as_posix():sha(path) for path in paths}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--binary',required=True,type=Path)
    parser.add_argument('--expected-sha',required=True)
    parser.add_argument('--source',required=True,type=Path)
    parser.add_argument('--features',default='all-features')
    parser.add_argument('--toolchain',default='1.95.0')
    args=parser.parse_args();binary=args.binary.resolve();source=args.source.resolve()
    if not re.fullmatch('[a-f0-9]{64}',args.expected_sha) or sha(binary)!=args.expected_sha:
        raise ValueError('binary differs from explicit approved SHA; witness refused')
    with binary.open('rb') as stream:
        if stream.read(4)!=b'\x7fELF':raise ValueError('POSIX rolling witness requires actual ELF binary')
    files=source_files(source);canonical=json.dumps(files,separators=(',',':'),sort_keys=True).encode();schema=re.search(r'pub const DATABASE_SCHEMA_VERSION: i64 = (\d+);',(source/'src/runtime/database.rs').read_text())
    if not schema:raise ValueError('source schema constant missing')
    report={'version':1,'status':'passed','completed':True,'kind':'host_frozen_source_build_witness','source_root':str(source),'source_files':files,'source_sha256':hashlib.sha256(canonical).hexdigest(),'schema_version':int(schema.group(1)),
            'features':args.features,'toolchain':args.toolchain,'rustc':subprocess.check_output(['rustc','+'+args.toolchain,'--version','--verbose'],text=True).strip(),
            'binary':{'path':str(binary),'sha256':args.expected_sha,'version':subprocess.check_output([str(binary),'--version'],text=True).strip()},'scope':__doc__}
    with args.out.open('x',encoding='utf-8') as stream:json.dump(report,stream,indent=2);stream.write('\n')
    print(json.dumps({'witness':str(args.out.resolve()),'witness_sha256':sha(args.out),'binary_sha256':args.expected_sha,'source_sha256':report['source_sha256']}))


if __name__=='__main__':main()
