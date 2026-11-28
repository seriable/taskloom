"""Create local Ed25519 publisher keys and signed skill-release metadata.

Requires explicitly installed Python cryptography (no implicit installation or
network). Private key files are create-only, mode0600 on POSIX, and never printed.
The Rust registry independently verifies signatures, trust, archive and package
identity before installation; this authoring helper never grants activation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PrivateFormat, PublicFormat, NoEncryption


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate metadata field')
        result[key] = value
    return result


def read_bytes(path, maximum):
    if path.is_symlink() or not path.is_file():
        raise ValueError('regular non-symlink file required')
    with path.open('rb') as source:
        value = source.read(maximum + 1)
    if len(value) > maximum:
        raise ValueError('authoring input exceeds limit')
    return value


def write_new(path, value, private=False):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    descriptor = os.open(path, flags, 0o600 if private else 0o644)
    with os.fdopen(descriptor, 'wb') as output:
        output.write(value)
        output.flush()
        os.fsync(output.fileno())


def signing_bytes(manifest):
    required = {'format', 'publisher', 'key_id', 'trust_revision', 'sequence', 'expires_unix', 'package', 'archive_sha256', 'archive_bytes'}
    if set(manifest) != required or manifest['format'] != 1:
        raise ValueError('exact format1 release fields required')
    for key in ['publisher', 'key_id']:
        if not isinstance(manifest[key], str) or not re.fullmatch('[a-z0-9-]{1,48}', manifest[key]):
            raise ValueError('invalid publisher/key identity')
    package = manifest['package']
    if set(package) != {'name', 'version', 'digest'} or not re.fullmatch('[a-z0-9-]{1,48}', package['name']):
        raise ValueError('invalid package identity')
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', package['version']) or len(package['version']) > 32 or any(int(part) > 2**32-1 for part in package['version'].split('.')):
        raise ValueError('canonical major.minor.patch version required')
    for value in [package['digest'], manifest['archive_sha256']]:
        if not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value):
            raise ValueError('lowercase SHA256 required')
    for key in ['trust_revision', 'sequence', 'expires_unix', 'archive_bytes']:
        if type(manifest[key]) is not int or not 1 <= manifest[key] <= 2**64-1:
            raise ValueError('positive u64 release integer required')
    if not 1024 <= manifest['archive_bytes'] <= 8*1024*1024 or manifest['archive_bytes'] % 512:
        raise ValueError('bounded USTAR archive bytes required')
    parts = ['taskloom-skill-registry-v1', 1, manifest['publisher'], manifest['key_id'], manifest['trust_revision'], manifest['sequence'], manifest['expires_unix'], package['name'], package['version'], package['digest'], manifest['archive_sha256'], manifest['archive_bytes']]
    return json.dumps(parts, separators=(',', ':'), ensure_ascii=True).encode('ascii')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    keygen = sub.add_parser('keygen')
    keygen.add_argument('--private-key', type=Path, required=True)
    keygen.add_argument('--public-key', type=Path, required=True)
    sign = sub.add_parser('sign')
    sign.add_argument('--private-key', type=Path, required=True)
    sign.add_argument('--manifest', type=Path, required=True)
    sign.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'keygen':
        key = Ed25519PrivateKey.generate()
        write_new(args.private_key, key.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption()), private=True)
        public = {'algorithm': 'Ed25519', 'public_key': key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw).hex()}
        write_new(args.public_key, (json.dumps(public, indent=2)+'\n').encode('utf-8'))
        print(json.dumps({'private_key_created': True, 'public_key_file': str(args.public_key)}))
    else:
        raw = read_bytes(args.private_key, 32)
        if len(raw) != 32:
            raise ValueError('exact32-byte raw private key required')
        manifest = json.loads(read_bytes(args.manifest, 128*1024), object_pairs_hook=unique_object)
        key = Ed25519PrivateKey.from_private_bytes(raw)
        output = {'manifest': manifest, 'signature': key.sign(signing_bytes(manifest)).hex()}
        value = (json.dumps(output, indent=2, ensure_ascii=False)+'\n').encode('utf-8')
        write_new(args.out, value)
        print(json.dumps({'signed_metadata_file': str(args.out), 'sha256': hashlib.sha256(value).hexdigest()}))


if __name__ == '__main__':
    main()
