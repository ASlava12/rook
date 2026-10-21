"""Owned independent-CLI cache writer checks; downloaded payloads never execute."""
import argparse
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__:
    parser.error('Run without Python optimization; assertions are verification.')
binary = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
if not binary.is_file() or not args.output.parent.is_dir() or args.output.exists():
    parser.error('Use an existing CLI and a fresh output path with an existing parent.')
before_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
payloads = {tag: (f'owned non-executable payload {tag}\n'.encode() * 524288) for tag in ['owned-a', 'owned-b']}
archives = {}
for tag, payload in payloads.items():
    memory = io.BytesIO()
    with zipfile.ZipFile(memory, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('rust-analyzer.exe', payload)
    archives[tag] = {'.zip': memory.getvalue(), '.gz': gzip.compress(payload)}


def preview(data):
    text = data.decode('utf-8', errors='replace')
    return text if len(text) <= 2400 else text[:1200] + '\n... output omitted ...\n' + text[-1200:]


def run_case(kind):
    state = {'metadata': 0, 'assets': 0}
    mutex = threading.Lock()
    first = threading.Event()
    both = threading.Event()
    release = threading.Event()

    class Service(http.server.BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(10)

        def log_message(self, *unused):
            pass

        def do_GET(self):
            if self.path.endswith('/releases/latest'):
                with mutex:
                    state['metadata'] += 1
                    assert state['metadata'] <= 2
                    tag = 'owned-b' if kind == 'versions' and state['metadata'] == 2 else 'owned-a'
                assets = []
                for arch in ['x86_64', 'aarch64']:
                    for target, suffix in [('pc-windows-msvc', '.zip'), ('apple-darwin', '.gz'),
                                           ('unknown-linux-gnu', '.gz'), ('unknown-linux-musl', '.gz')]:
                        name = f'rust-analyzer-{arch}-{target}{suffix}'
                        body = archives[tag][suffix]
                        digest = '0' * 64 if kind == 'bad-digest' else hashlib.sha256(body).hexdigest()
                        assets.append({'name': name, 'size': len(body), 'digest': 'sha256:' + digest,
                                       'browser_download_url': f'http://127.0.0.1:{self.server.server_port}/asset/{tag}/{name}'})
                body = json.dumps({'tag_name': tag, 'assets': assets}).encode()
            else:
                parts = self.path.split('/')
                assert len(parts) == 4 and parts[1] == 'asset' and parts[2] in archives
                suffix = '.zip' if parts[3].endswith('.zip') else '.gz'
                with mutex:
                    state['assets'] += 1
                    assert state['assets'] <= 2
                    first.set()
                    if state['assets'] == 2:
                        both.set()
                assert release.wait(40), 'owned asset gate deadline'
                body = archives[parts[2]][suffix]
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    temporary = tempfile.TemporaryDirectory(prefix='rook-install-publication-')
    root = Path(temporary.name)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    processes = []
    try:
        home = root / 'home'
        home.mkdir()
        workspace = root / 'workspace'
        workspace.mkdir()
        empty = root / 'empty'
        empty.mkdir()
        (home / 'config.toml').write_text('[agent]\ninstall_servers=false\n', encoding='utf-8')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_BUILTIN_SKILLS=str(empty),
                   ROOK_RELEASE_API=f'http://127.0.0.1:{server.server_port}', ROOK_LOG='error', PATH=str(empty))
        argv = [str(binary), '--workspace', str(workspace), 'lsp', 'install', 'rust-analyzer']

        def start():
            processes.append(subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                              creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0)))

        start()
        assert first.wait(15), 'first owned asset request not reached'
        overlap = False
        if kind != 'bad-digest':
            start()
            overlap = both.wait(15)
            assert overlap, 'second asset must be requested before the first is released'
        release.set()
        outputs = []
        for process in processes:
            out, err = process.communicate(timeout=30)
            outputs.append({'pid': process.pid, 'exit': process.returncode, 'stdout': preview(out), 'stderr': preview(err)})
        cache = home / 'servers'
        current = cache / 'rust-analyzer' / 'current'
        placed = current / ('rust-analyzer.exe' if os.name == 'nt' else 'rust-analyzer')
        tag_path = current / '.tag'
        tag = tag_path.read_text(encoding='utf-8').strip() if tag_path.is_file() else None
        matched = tag in payloads and placed.is_file() and hashlib.sha256(placed.read_bytes()).digest() == hashlib.sha256(payloads[tag]).digest()
        expected_tags = ['owned-a', 'owned-b'] if kind == 'versions' else ['owned-a', 'owned-a']
        receipts = all(f'rust-analyzer {expected_tag} ' in result['stdout'] and 'verified:' in result['stdout']
                       for result, expected_tag in zip(outputs, expected_tags))
        return {'counts': dict(state), 'overlap': overlap, 'outputs': outputs, 'tag': tag,
                'payload_matches_tag': matched, 'caller_releases': receipts,
                'cache_exists': cache.exists(), 'incoming': [str(path.relative_to(cache)) for path in cache.rglob('*.incoming')]}
    finally:
        release.set()
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        temporary.cleanup()


cases = {kind: run_case(kind) for kind in ['identical', 'versions', 'bad-digest']}
paired = [cases['identical'], cases['versions']]
refused = cases['bad-digest']
checks = {
    'independent_processes': all(len(case['outputs']) == 2 and case['outputs'][0]['pid'] != case['outputs'][1]['pid'] for case in paired),
    'both_downloads_overlap': all(case['overlap'] and case['counts'] == {'metadata': 2, 'assets': 2} for case in paired),
    'same_version_both_succeed': all(result['exit'] == 0 for result in cases['identical']['outputs']),
    'different_versions_both_succeed': all(result['exit'] == 0 for result in cases['versions']['outputs']),
    'caller_results_keep_own_release': all(case['caller_releases'] for case in paired),
    'final_tag_matches_checked_payload': all(case['payload_matches_tag'] for case in paired),
    'bad_digest_leaves_no_cache': (len(refused['outputs']) == 1 and refused['outputs'][0]['exit'] != 0
        and 'does not match' in refused['outputs'][0]['stderr'] and not refused['cache_exists']
        and refused['counts'] == {'metadata': 1, 'assets': 1}),
    'no_pending_publication_files': all(not case['incoming'] for case in paired),
}
after_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
assert before_hash == after_hash
report = {'binary': str(binary), 'binary_sha256': before_hash, 'checks': checks, 'cases': cases}
with args.output.open('x', encoding='utf-8') as sink:
    json.dump(report, sink, indent=2)
print(json.dumps({'binary_sha256': before_hash, 'checks': checks}), flush=True)
assert all(checks.values()), checks
