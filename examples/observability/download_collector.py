"""Download a pinned official Windows Collector for local protocol acceptance.

No installer or extraction hooks. Writes only the archive and one regular EXE
in the explicit create-only destination; the binary is never part of source
delivery. The public release API needs no credentials.
"""
import argparse
import hashlib
import json
from pathlib import Path
import tarfile
import urllib.request
from urllib.parse import urlsplit

RELEASE = 'v0.140.0'
ASSET = 'otelcol_0.140.0_windows_amd64.tar.gz'
SIZE = 43128491
SHA256 = '68239baf58f361cb03d5dd1e66fe334ee79d5ad99c3c6d6f01d90101ecd62c45'


class ReleaseRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        url = urlsplit(newurl)
        if url.scheme != 'https' or url.hostname not in {'api.github.com', 'github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com'}:
            raise ValueError('unexpected release redirect destination')
        return super().redirect_request(request, fp, code, msg, headers, newurl)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    args.destination.mkdir(parents=True, exist_ok=False)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), ReleaseRedirect())
    endpoint = 'https://api.github.com/repos/open-telemetry/opentelemetry-collector-releases/releases/tags/' + RELEASE
    with opener.open(urllib.request.Request(endpoint, headers={'User-Agent': 'taskloom-local-acceptance'}), timeout=30) as response:
        raw = response.read(2 * 1024 * 1024 + 1)
        if len(raw) > 2 * 1024 * 1024:
            raise ValueError('release metadata exceeds limit')
        release = json.loads(raw)
    asset = next(a for a in release['assets'] if a['name'] == ASSET)
    if asset['size'] != SIZE or asset['digest'] != 'sha256:' + SHA256:
        raise ValueError('official asset does not match this explicitly pinned release')
    archive = args.destination / ASSET
    request = urllib.request.Request(asset['url'] + '?taskloom-download=1', headers={'User-Agent': 'taskloom-local-acceptance', 'Accept': 'application/octet-stream'})
    count = 0
    with opener.open(request, timeout=30) as response, archive.open('xb') as stream:
        while chunk := response.read(1024 * 1024):
            count += len(chunk)
            if count > SIZE:
                raise ValueError('release exceeds pinned size')
            stream.write(chunk)
    if count != SIZE or hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise ValueError('release size/hash mismatch; failed artifact retained')
    binary = args.destination / 'otelcol.exe'
    with tarfile.open(archive) as package:
        files = [m for m in package if m.name == 'otelcol.exe']
        if len(files) != 1 or not files[0].isfile() or not 0 < files[0].size < 256 * 1024 * 1024:
            raise ValueError('expected one bounded regular Collector executable')
        with package.extractfile(files[0]) as source, binary.open('xb') as destination:
            while chunk := source.read(1024 * 1024):
                destination.write(chunk)
    provenance = {'release': RELEASE, 'asset': ASSET, 'archive_sha256': SHA256,
                  'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                  'note': 'This release binary self-identifies as 0.140.1; retain its actual --version output.'}
    (args.destination / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(provenance))


if __name__ == '__main__':
    main()
