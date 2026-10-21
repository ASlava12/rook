"""Check packaged CLI release-tag refusals against an owned local API."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__:
    parser.error('Run without Python optimization.')
if args.output.exists() or not args.output.parent.is_dir():
    parser.error('Choose a fresh output file in an existing directory.')
binary = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
assert binary.is_file()
cases = {f'{command}-{kind}': (command, tag) for command in ['lsp', 'update']
         for kind, tag in [('traversal', '../../sentinel'), ('device', 'CON')]}
latest = dict.fromkeys(cases, 0)
downloads = []
class Api(http.server.BaseHTTPRequestHandler):
    def log_message(self, *unused):
        pass
    def do_GET(self):
        key = self.path.split('/')[1]
        if key in cases and self.path.endswith('/releases/latest'):
            latest[key] += 1
            names = [f'rust-analyzer-{arch}-{target}{suffix}' for arch in ['x86_64', 'aarch64']
                     for target, suffix in [('pc-windows-msvc', '.zip'), ('apple-darwin', '.gz'),
                                            ('unknown-linux-gnu', '.gz'), ('unknown-linux-musl', '.gz')]]
            assets = [{'name': name, 'size': 1, 'digest': 'sha256:' + '0' * 64,
                       'browser_download_url': f'http://127.0.0.1:{self.server.server_port}/download/{key}/{name}'}
                      for name in names]
            body = json.dumps({'tag_name': cases[key][1], 'assets': assets}).encode()
            status = 200
        else:
            if len(downloads) < 16:
                downloads.append(self.path[:1000])
            body, status = b'asset must not be requested', 404
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Api)
threading.Thread(target=server.serve_forever, daemon=True).start()
checks, details = {}, {}
try:
    with tempfile.TemporaryDirectory(prefix='rook-release-metadata-') as directory:
        root = Path(directory)
        workspace = root / 'workspace'; workspace.mkdir()
        empty = root / 'builtin-empty'; empty.mkdir()
        intact = True
        for key, (command, _) in cases.items():
            home = root / key; home.mkdir()
            sentinel = home / 'sentinel'; sentinel.write_bytes(b'owned sentinel')
            (home / 'config.toml').write_text("[agent]\ninstall_servers=false\n", encoding='utf-8')
            env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
                       ROOK_BUILTIN_SKILLS=str(empty), ROOK_LOG='error',
                       ROOK_RELEASE_API=f'http://127.0.0.1:{server.server_port}/{key}')
            options = ['lsp', 'install', 'rust-analyzer'] if command == 'lsp' else ['update', '--check']
            capture = root / (key + '.txt')
            with capture.open('wb') as sink:
                result = subprocess.run([str(binary), *options], env=env, cwd=workspace,
                                        stdout=sink, stderr=subprocess.STDOUT, timeout=90)
            assert capture.stat().st_size <= 64 * 1024
            text = capture.read_text(encoding='utf-8')
            checks[key] = result.returncode != 0 and 'portable directory name' in text and latest[key] == 1
            intact &= (sentinel.is_file() and sentinel.stat().st_size == len(b'owned sentinel')
                       and sentinel.read_bytes() == b'owned sentinel')
            details[key] = {'exit': result.returncode, 'api_requests': latest[key], 'output': text[:2000]}
        checks['no_asset_downloads'] = not downloads
        checks['owned_sentinels_unchanged'] = intact
    with binary.open('rb') as executable:
        digest = hashlib.file_digest(executable, 'sha256').hexdigest()
    report = {'checks': checks, 'binary': str(binary), 'binary_sha256': digest,
              'cases': details, 'asset_requests': downloads,
              'scope': 'Owned CLI metadata refusal only; no real download/install/update or cached Check application.'}
    args.output.write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({'checks': checks, 'binary_sha256': digest}), flush=True)
    assert all(checks.values()), report
finally:
    server.shutdown(); server.server_close()
