"""Author a deterministic data-only USTAR skill archive from declared assets.

Run on trusted authoring sources. The Rust installer independently validates
every archive, manifest and hash; this helper is not an activation/grant API.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import stat
import tarfile


def pack(directory, output):
    root = directory.resolve(strict=True)
    manifest = json.loads((root / 'skill.json').read_text(encoding='utf-8'))
    contents = {'skill.json': (root / 'skill.json').read_bytes()}
    for name, expected in manifest['files'].items():
        relative = Path(name)
        if relative.is_absolute() or '\\' in name or any(p in {'', '.', '..'} for p in name.split('/')):
            raise ValueError('unsafe declared asset path')
        path = root
        for part in relative.parts:
            path = path / part
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, 'st_file_attributes', 0) & 0x400:
                raise ValueError('symlink/reparse-point sources are forbidden')
        if not path.resolve(strict=True).is_relative_to(root):
            raise ValueError('asset leaves authoring directory')
        data = path.read_bytes()
        if len(data) > 256 * 1024 or hashlib.sha256(data).hexdigest() != expected:
            raise ValueError('asset size/hash mismatch: ' + name)
        contents[name] = data
    if len(contents) > 129 or sum(map(len, contents.values())) > 4 * 1024 * 1024:
        raise ValueError('package exceeds limits')
    # Create only. Never overwrite an existing release archive.
    with output.open('xb') as stream:
        with tarfile.open(fileobj=stream, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, data in sorted(contents.items()):
                header = tarfile.TarInfo(name)
                header.size = len(data)
                header.mode = 0o644
                header.mtime = 0
                archive.addfile(header, io.BytesIO(data))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    pack(args.directory, args.output)
