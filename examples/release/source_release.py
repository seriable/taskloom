"""Offline source-release preparation, exact checksums and Ed25519 signatures.

Python 3.11+ and explicitly installed cryptography are required. This never
creates keys, commits, publishes, builds/runs source or contacts a registry.
Trust is the caller's explicit raw public key, never a key from the archive.
"""
import argparse
import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tarfile
import threading
import tomllib

from cryptography.exceptions import InvalidSignature, UnsupportedAlgorithm
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat


DOMAIN = b'taskloom-source-release-ed25519-v1\x00'
MAX_ARTIFACT = 16 * 1024 * 1024
MAX_SOURCE = 64 * 1024 * 1024
MAX_TAR = 80 * 1024 * 1024
MAX_META = 4 * 1024 * 1024
MAX_FILES = 4096
ARTIFACT = 'source.tar.gz'
MANIFEST = 'source-manifest.json'
CHECKSUM = 'SHA256SUMS'


class Rejected(ValueError):
    """Only static, secret-free messages cross the CLI boundary."""


def reject(message):
    raise Rejected(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False,
                       allow_nan=False) + '\n').encode('utf-8')


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            reject('duplicate JSON field')
        result[key] = value
    return result


def json_input(data):
    try:
        return json.loads(data.decode('utf-8'), object_pairs_hook=unique,
                          parse_constant=lambda _: reject('nonfinite JSON value'))
    except Rejected:
        raise
    except (UnicodeError, ValueError, RecursionError):
        reject('invalid bounded UTF-8 JSON')


def regular_bytes(path, maximum):
    """No keys, archives or metadata are followed through a final symlink."""
    try:
        initial = path.lstat()
        if not stat.S_ISREG(initial.st_mode):
            reject('regular non-symlink input required')
        flags = os.O_RDONLY | getattr(os, 'O_BINARY', 0) | getattr(os, 'O_NOFOLLOW', 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, 'rb') as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode) or (initial.st_dev, initial.st_ino) != (opened.st_dev, opened.st_ino):
                reject('input identity changed')
            data = stream.read(maximum + 1)
            after = os.fstat(stream.fileno())
            if (opened.st_size, opened.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                reject('input changed during read')
    except OSError:
        reject('bounded input unavailable')
    if len(data) > maximum:
        reject('input exceeds fixed byte limit')
    return data


def write_new(path, data):
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, 'O_BINARY', 0), 0o644)
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except OSError:
        reject('create-only output unavailable or already exists')


def git(repo, arguments, maximum=MAX_META, allowed=(0,)):
    # Read objects/index directly. Do not run status/diff/filters/hooks, archive
    # attributes, source scripts, Git credential helpers or remote commands.
    environment = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    environment.update(GIT_TERMINAL_PROMPT='0', GIT_OPTIONAL_LOCKS='0', GIT_CONFIG_NOSYSTEM='1',
                       GIT_CONFIG_GLOBAL=os.devnull, GIT_NO_REPLACE_OBJECTS='1',
                       GIT_NO_LAZY_FETCH='1', GIT_ALLOW_PROTOCOL='', GIT_PROTOCOL_FROM_USER='0')
    command = ['git', '--no-optional-locks', '-c', 'core.fsmonitor=false', '-C', str(repo), *arguments]
    try:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, env=environment,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    except OSError:
        reject('local Git unavailable')
    data = bytearray()
    failed = []

    def drain():
        try:
            while True:
                chunk = process.stdout.read(8192)
                if not chunk:
                    break
                if len(data) + len(chunk) > maximum:
                    failed.append(True)
                    process.kill()
                    break
                data.extend(chunk)
        except OSError:
            failed.append(True)
        finally:
            process.stdout.close()

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        code = process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
        failed.append(True)
        code = -1
    reader.join(timeout=5)
    if reader.is_alive() or failed or code not in allowed:
        reject('bounded local Git operation failed')
    return bytes(data), code


def text_git(repo, arguments):
    data, _ = git(repo, arguments)
    try:
        return data.decode('utf-8').strip()
    except UnicodeError:
        reject('Git metadata must be UTF-8')


def safe_path(value):
    return (value and len(value.encode('utf-8')) <= 1024 and not value.startswith('/')
            and not any(c in value for c in '\\:\x00') and not any(ord(c) < 32 or ord(c) == 127 for c in value)
            and all(part not in ('', '.', '..', '.git') for part in value.split('/')))


def source_path(value):
    if not safe_path(value):
        return False
    parts = value.lower().split('/')
    forbidden = {'target', 'node_modules', '__pycache__', '.venv', '.taskloom'}
    suffixes = {'.exe', '.dll', '.so', '.dylib', '.pdb', '.pyc', '.pyo', '.whl', '.crate',
                '.class', '.jar', '.key', '.pem', '.p12', '.pfx', '.db', '.sqlite', '.sqlite3'}
    name = parts[-1]
    return (not any(part in forbidden for part in parts) and Path(name).suffix not in suffixes
            and name != '.env' and not name.startswith('.env.')
            and name not in ('id_rsa', 'id_ed25519', 'credentials.json'))


def repository(path):
    try:
        repo = path.resolve(strict=True)
    except OSError:
        reject('source repository unavailable')
    if text_git(repo, ['rev-parse', '--is-inside-work-tree']) != 'true':
        reject('explicit Git worktree required')
    try:
        root = Path(text_git(repo, ['rev-parse', '--show-toplevel'])).resolve(strict=True)
    except OSError:
        reject('Git worktree root unavailable')
    if root != repo:
        reject('pass the exact source worktree root')
    return repo


def source_snapshot(repo):
    commit = text_git(repo, ['rev-parse', '--verify', 'HEAD^{commit}'])
    tree = text_git(repo, ['rev-parse', '--verify', 'HEAD^{tree}'])
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit) or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', tree):
        reject('unsupported Git object identity')
    try:
        timestamp = int(text_git(repo, ['show', '-s', '--format=%ct', commit]))
    except ValueError:
        reject('invalid source commit time')
    if not 0 <= timestamp <= 0xffffffff:
        reject('source commit time outside release profile')
    raw_tree, _ = git(repo, ['ls-tree', '-r', '-z', '--full-tree', commit])
    entries = []
    seen = set()
    for raw in raw_tree.split(b'\x00'):
        if not raw:
            continue
        try:
            header, name = raw.split(b'\t', 1)
            mode, kind, object_id = header.decode('ascii').split(' ')
            name = name.decode('utf-8')
        except (ValueError, UnicodeError):
            reject('invalid source tree entry')
        if (mode not in ('100644', '100755') or kind != 'blob' or not source_path(name)
                or name.casefold() in seen or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', object_id)):
            reject('source tree has unsafe/duplicate/link/submodule entries')
        seen.add(name.casefold())
        entries.append((name, mode, object_id))
        if len(entries) > MAX_FILES:
            reject('source tree file count exceeds limit')
    if not entries:
        reject('source tree is empty')
    entries.sort()
    index_raw, _ = git(repo, ['ls-files', '--stage', '-z'])
    index = []
    for raw in index_raw.split(b'\x00'):
        if not raw:
            continue
        try:
            header, name = raw.split(b'\t', 1)
            mode, object_id, stage = header.decode('ascii').split(' ')
            if stage != '0':
                reject('unmerged source index')
            index.append((name.decode('utf-8'), mode, object_id))
        except (ValueError, UnicodeError):
            reject('invalid source index')
    if sorted(index) != entries:
        reject('source index differs from HEAD tree')
    untracked, _ = git(repo, ['ls-files', '--others', '--exclude-standard', '-z'])
    if untracked:
        reject('untracked non-ignored source files remain')
    files = []
    total = 0
    for name, mode, object_id in entries:
        try:
            size = int(text_git(repo, ['cat-file', '-s', object_id]))
        except ValueError:
            reject('invalid Git blob size')
        total += size
        if size < 0 or size > MAX_ARTIFACT or total > MAX_SOURCE:
            reject('source file/aggregate bytes exceed limit')
        data, _ = git(repo, ['cat-file', 'blob', object_id], maximum=size)
        path = repo.joinpath(*name.split('/'))
        # A tracked file reached via a replaced directory symlink is not clean.
        for parent in path.parents:
            if parent == repo:
                break
            if parent.is_symlink():
                reject('source parent became a symlink')
        if regular_bytes(path, MAX_ARTIFACT) != data:
            reject('raw worktree source differs from HEAD blob')
        if os.name != 'nt' and bool(path.stat().st_mode & 0o111) != (mode == '100755'):
            reject('worktree executable mode differs from source tree')
        files.append({'path': name, 'mode': mode, 'bytes': len(data), 'sha256': sha(data), 'data': data})
    if (text_git(repo, ['rev-parse', '--verify', 'HEAD^{commit}']) != commit
            or git(repo, ['ls-files', '--stage', '-z'])[0] != index_raw
            or git(repo, ['ls-files', '--others', '--exclude-standard', '-z'])[0]):
        reject('source Git state changed during preparation')
    cargo = next((file['data'] for file in files if file['path'] == 'Cargo.toml'), None)
    if cargo is None:
        reject('source Cargo.toml required')
    try:
        cargo = tomllib.loads(cargo.decode('utf-8'))
        package = cargo['package']
        if package['rust-version'] != '1.86' or cargo['features']['default'] != []:
            reject('release profile requires actual Rust 1.86 and default features []')
        if not isinstance(package['name'], str) or not re.fullmatch('[a-zA-Z0-9_-]{1,64}', package['name']):
            reject('invalid source package name')
        if not isinstance(package['version'], str) or not re.fullmatch(r'[0-9A-Za-z.+-]{1,64}', package['version']):
            reject('invalid source package version')
    except (UnicodeError, tomllib.TOMLDecodeError, KeyError, TypeError):
        reject('unsupported source Cargo metadata')
    return {'commit': commit, 'tree': tree, 'clean': True}, timestamp, files, package


def source_tar(files, timestamp):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w', format=tarfile.PAX_FORMAT) as archive:
        for file in files:
            info = tarfile.TarInfo('source/' + file['path'])
            info.size = file['bytes']
            info.mode = 0o755 if file['mode'] == '100755' else 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = ''
            info.mtime = timestamp
            archive.addfile(info, io.BytesIO(file['data']))
    data = buffer.getvalue()
    if len(data) > MAX_TAR:
        reject('source tar exceeds bound')
    return data


def compressed(data):
    buffer = io.BytesIO()
    with gzip.GzipFile(fileobj=buffer, mode='wb', filename='', mtime=0, compresslevel=9) as archive:
        archive.write(data)
    data = buffer.getvalue()
    if not 1 <= len(data) <= MAX_ARTIFACT:
        reject('compressed source artifact exceeds 16 MiB')
    return data


def manifest(source, files, package, artifact, tar_bytes):
    return {
        'format': 'taskloom-source-release-v1', 'source': source,
        'package': {'name': package['name'], 'version': package['version'], 'msrv': '1.86', 'default_features': []},
        'artifact': {'name': ARTIFACT, 'format': 'canonical-source-pax-tar-gzip-v1', 'bytes': len(artifact),
                     'sha256': sha(artifact), 'uncompressed_tar_bytes': len(tar_bytes)},
        'files': [{key: file[key] for key in ('path', 'mode', 'bytes', 'sha256')} for file in files],
        'claims': {'source_identity_and_exact_bytes_only': True, 'quality_or_test_attestation': False,
                   'maintainer_identity_discovered_from_artifact': False},
    }


def allowed_output(repo, output):
    output = output.resolve()
    try:
        relative = output.relative_to(repo)
    except ValueError:
        return output
    _, code = git(repo, ['check-ignore', '--quiet', '--no-index', '--', relative.as_posix()], allowed=(0, 1))
    if code != 0:
        reject('release outputs inside source tree must be explicitly Git-ignored')
    return output


def prepare(repo_path, output):
    repo = repository(repo_path)
    output = allowed_output(repo, output)
    source, timestamp, files, package = source_snapshot(repo)
    tar_bytes = source_tar(files, timestamp)
    artifact = compressed(tar_bytes)
    document = manifest(source, files, package, artifact, tar_bytes)
    encoded = canonical(document)
    if len(encoded) > MAX_META:
        reject('source manifest exceeds bound')
    # Immutable blobs are used for the archive. Reject any observed drift before
    # creating outputs; this is not a distributed/atomic worktree freeze service.
    source_after, _, files_after, _ = source_snapshot(repo)
    if source_after != source or files_after != files:
        reject('source changed before output publication')
    try:
        output.mkdir(parents=False, exist_ok=False)
    except OSError:
        reject('create-only bundle directory unavailable or already exists')
    write_new(output / ARTIFACT, artifact)
    write_new(output / MANIFEST, encoded)
    write_new(output / CHECKSUM, f'{sha(artifact)}  {ARTIFACT}\n'.encode('ascii'))
    return document


def verified_bundle(repo_path, bundle):
    repo = repository(repo_path)
    artifact = regular_bytes(bundle / ARTIFACT, MAX_ARTIFACT)
    encoded = regular_bytes(bundle / MANIFEST, MAX_META)
    declared = json_input(encoded)
    if canonical(declared) != encoded:
        reject('source manifest is not canonical release JSON')
    source, timestamp, files, package = source_snapshot(repo)
    tar_bytes = source_tar(files, timestamp)
    # Do not extract or trust paths from an archive. Fully bounded decompression
    # must exactly equal the tar reconstructed from actual clean Git objects.
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(artifact), mode='rb') as stream:
            actual_tar = stream.read(MAX_TAR + 1)
    except (OSError, EOFError, ValueError):
        reject('source artifact gzip rejected')
    if actual_tar != tar_bytes:
        reject('source artifact does not exactly represent this clean Git tree')
    expected = manifest(source, files, package, artifact, tar_bytes)
    if declared != expected:
        reject('source manifest differs from actual Git/package/artifact contract')
    if regular_bytes(bundle / CHECKSUM, 256) != f'{sha(artifact)}  {ARTIFACT}\n'.encode('ascii'):
        reject('source checksum file differs from exact artifact bytes')
    return encoded, artifact, expected


def sign(repo, bundle, private_path, output):
    encoded, artifact, document = verified_bundle(repo, bundle)
    if private_path.resolve().is_relative_to(bundle.resolve()):
        reject('private key must remain outside the release bundle')
    root = repository(repo)
    output = allowed_output(root, output)
    if any(private_path.resolve() == root.joinpath(*file['path'].split('/')).resolve()
           for file in document['files']):
        reject('private key must not be tracked source content')
    key_bytes = regular_bytes(private_path, 32)
    if len(key_bytes) != 32:
        reject('explicit raw 32-byte Ed25519 private key required')
    try:
        private = Ed25519PrivateKey.from_private_bytes(key_bytes)
        public = private.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        signature = private.sign(DOMAIN + encoded)
    except (ValueError, UnsupportedAlgorithm):
        reject('Ed25519 signing unavailable or invalid host key')
    packet = {'format': 'taskloom-source-release-signature-v1', 'algorithm': 'Ed25519',
              'domain': DOMAIN[:-1].decode('ascii'), 'manifest_sha256': sha(encoded),
              'artifact_sha256': sha(artifact), 'public_key_sha256': sha(public),
              'signature_base64': base64.b64encode(signature).decode('ascii')}
    write_new(output, canonical(packet))
    return {'signed_exact_source': True, 'source_commit': document['source']['commit'],
            'public_key_sha256': sha(public), 'quality_or_maintainer_identity_verified': False}


def verify(repo, bundle, signature_path, public_path):
    encoded, artifact, document = verified_bundle(repo, bundle)
    if public_path.resolve().is_relative_to(bundle.resolve()):
        reject('explicit trust key must come from outside the release bundle')
    raw = regular_bytes(signature_path, 4096)
    packet = json_input(raw)
    fields = {'format', 'algorithm', 'domain', 'manifest_sha256', 'artifact_sha256', 'public_key_sha256', 'signature_base64'}
    if not isinstance(packet, dict) or set(packet) != fields or canonical(packet) != raw:
        reject('exact canonical source signature fields required')
    public = regular_bytes(public_path, 32)
    if len(public) != 32:
        reject('explicit raw 32-byte Ed25519 public key required')
    if (packet['format'] != 'taskloom-source-release-signature-v1' or packet['algorithm'] != 'Ed25519'
            or packet['domain'] != DOMAIN[:-1].decode('ascii') or packet['manifest_sha256'] != sha(encoded)
            or packet['artifact_sha256'] != sha(artifact) or packet['public_key_sha256'] != sha(public)):
        reject('source signature domain/key/manifest/artifact binding mismatch')
    try:
        signature = base64.b64decode(packet['signature_base64'], validate=True)
        if len(signature) != 64:
            reject('invalid Ed25519 signature length')
        Ed25519PublicKey.from_public_bytes(public).verify(signature, DOMAIN + encoded)
    except (InvalidSignature, UnsupportedAlgorithm, ValueError, TypeError):
        reject('explicit public key did not verify exact source release')
    return {'verified_exact_source': True, 'source_commit': document['source']['commit'],
            'source_tree': document['source']['tree'], 'artifact_sha256': sha(artifact),
            'public_key_sha256': sha(public), 'quality_or_maintainer_identity_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    create = commands.add_parser('prepare')
    create.add_argument('--repo', type=Path, required=True)
    create.add_argument('--out', type=Path, required=True)
    for name in ('sign', 'verify'):
        command = commands.add_parser(name)
        command.add_argument('--repo', type=Path, required=True)
        command.add_argument('--bundle', type=Path, required=True)
        command.add_argument('--private-key' if name == 'sign' else '--public-key', type=Path, required=True)
        command.add_argument('--out' if name == 'sign' else '--signature', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == 'prepare':
            prepared = prepare(args.repo, args.out)
            result = {'prepared_exact_source': True, 'source_commit': prepared['source']['commit'],
                      'source_tree': prepared['source']['tree'], 'artifact_sha256': prepared['artifact']['sha256'],
                      'artifact_bytes': prepared['artifact']['bytes'], 'source_files': len(prepared['files']),
                      'quality_or_maintainer_identity_verified': False}
        elif args.command == 'sign':
            result = sign(args.repo, args.bundle, args.private_key, args.out)
        else:
            result = verify(args.repo, args.bundle, args.signature, args.public_key)
    except Rejected as failure:
        parser.exit(2, f'Source release rejected: {failure}\n')
    print(json.dumps(result, ensure_ascii=False, allow_nan=False))


if __name__ == '__main__':
    main()
