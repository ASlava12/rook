"""Actual CLI/daemon replay of valid legacy bare-image user messages."""
import argparse, hashlib, http.server, json, os, shutil, subprocess, tempfile, threading, time, tomllib, urllib.request
from pathlib import Path
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
parser.add_argument('--build-target', type=Path)
args = parser.parse_args()
source = Path(__file__).resolve().parents[3]
scripts = Path(__file__).resolve().parent
suffix = '.exe' if os.name == 'nt' else ''
output = args.output.resolve()
bins = args.bin_dir.resolve()
cargo = shutil.which('cargo')
compiler = shutil.which('rustc')
node = shutil.which('node')
assert __debug__ and cargo and compiler and node and (not output.exists()) and output.parent.is_dir()
for name in ['rook', 'rookd']:
    assert (bins / (name + suffix)).is_file()
root = Path(tempfile.mkdtemp(prefix='rook-anthropic-image-only-', dir=(args.tmp_root or Path(tempfile.gettempdir())).resolve())).resolve()
for name in ['helper', 'tools', 'empty-skills', 'logs']:
    (root / name).mkdir()

def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
binaries = {name: {'path': str(bins / (name + suffix)), 'sha256': sha(bins / (name + suffix))} for name in ['rook', 'rookd']}
report = {'status': 'incomplete', 'owned_directory': str(root), 'binaries': binaries, 'source_inputs': {name: sha(source / name) for name in ['Cargo.lock', 'xtask/probes/anthropic-image-only/seed.rs', 'xtask/probes/anthropic-image-only/client.mjs', 'xtask/probes/anthropic-image-only/run.py']}, 'cases': []}

def run(command, stem, env, timeout=120, cwd=None):
    child = subprocess.Popen(command, env=env, cwd=cwd or root, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    captures = {'stdout': bytearray(), 'stderr': bytearray()}
    overflow = threading.Event()

    def drain(name, stream):
        while (chunk := stream.read1(4096)):
            room = 65536 - len(captures[name])
            captures[name].extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if child.poll() is None:
                    child.kill()
    readers = [threading.Thread(target=drain, args=(name, stream), daemon=True) for name, stream in [('stdout', child.stdout), ('stderr', child.stderr)]]
    for reader in readers:
        reader.start()
    try:
        code = child.wait(timeout=timeout)
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        for reader in readers:
            reader.join(timeout=10)
        assert not any((reader.is_alive() for reader in readers)), 'owned capture reader did not retire'
        child.stdout.close()
        child.stderr.close()
    for name, data in captures.items():
        (root / 'logs' / (stem + '.' + name)).write_bytes(data)
    assert not overflow.is_set(), 'owned command exceeded64KiB capture per stream'
    return (code, bytes(captures['stdout']), bytes(captures['stderr']))
wire = []
errors = []
lock = threading.Lock()
markers = ['OWNED_IMAGE_CLI_5M', 'OWNED_IMAGE_DAEMON_1H']

class Model(http.server.BaseHTTPRequestHandler):

    def log_message(self, *unused):
        pass

    def do_POST(self):
        try:
            self.connection.settimeout(30)
            size = int(self.headers.get('Content-Length', '0'))
            assert 0 < size <= 1024 * 1024
            body = json.loads(self.rfile.read(size))
            text = json.dumps(body)
            found = [m for m in markers if m in text]
            assert len(found) == 1
            with lock:
                assert len(wire) < 8
                checking = 'Classify whether an assistant' in text
                wire.append({'marker': found[0], 'checking': checking, 'path': self.path, 'auth_owned': self.headers.get('x-api-key') == 'owned-native-image-key', 'body': body})
            answer = '{"action":"finish"}' if checking else 'owned image replay done'
            if body.get('stream'):
                events = [{'type': 'message_start', 'message': {'id': 'owned', 'type': 'message', 'role': 'assistant', 'model': 'claude-opus-5', 'content': [], 'usage': {'input_tokens': 20, 'output_tokens': 0}}}, {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}, {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': answer}}, {'type': 'content_block_stop', 'index': 0}, {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 4}}, {'type': 'message_stop'}]
                data = ''.join(('data: ' + json.dumps(e) + '\n\n' for e in events)).encode()
                mime = 'text/event-stream'
            else:
                data = json.dumps({'id': 'owned', 'model': 'claude-opus-5', 'content': [{'type': 'text', 'text': answer}], 'stop_reason': 'end_turn', 'usage': {'input_tokens': 20, 'output_tokens': 4}}).encode()
                mime = 'application/json'
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()
        except Exception as error:
            with lock:
                if len(errors) < 8:
                    errors.append(repr(error)[:1000])
            self.send_error(500)

    def do_GET(self):
        with lock:
            errors.append('unexpected model GET ' + self.path[:200])
        self.send_error(500)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
daemon = None
daemon_logs = []
try:
    print(json.dumps({'phase': 'seed-helper-build'}), flush=True)
    project = root / 'helper'
    shutil.copy2(scripts / 'seed.rs', project / 'main.rs')
    shutil.copy2(source / 'Cargo.lock', project / 'Cargo.lock')
    manifest = '[package]\nname="rook-anthropic-image-seed"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nrook-store={path=' + json.dumps(str(source / 'crates/rook-store')) + '}\nserde_json="1"\n[[bin]]\nname="rook-anthropic-image-seed"\npath="main.rs"\n'
    (project / 'Cargo.toml').write_text(manifest, encoding='utf-8')
    target = (args.build_target or root / 'build-target').resolve()
    build_env = dict(os.environ, CARGO_INCREMENTAL='0', CARGO_BUILD_JOBS='3', CARGO_PROFILE_DEV_DEBUG='0')
    code, out, err = run([cargo, 'build', '--offline', '--manifest-path', str(project / 'Cargo.toml'), '--target-dir', str(target)], 'seed-build', build_env, 600)
    assert code == 0, err[-3000:]
    original = tomllib.loads((source / 'Cargo.lock').read_text(encoding='utf-8'))['package']
    allowed = {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in original}
    resolved = tomllib.loads((project / 'Cargo.lock').read_text(encoding='utf-8'))['package']
    assert all(((p['name'], p['version'], p.get('source'), p.get('checksum')) in allowed for p in resolved if p['name'] != 'rook-anthropic-image-seed'))
    seed = root / ('seed' + suffix)
    shutil.copy2(target / 'debug' / ('rook-anthropic-image-seed' + suffix), seed)
    report['seed_binary_sha256'] = sha(seed)
    placeholder = root / 'inventory.rs'
    placeholder.write_text('#![cfg_attr(windows, windows_subsystem="windows")]\nfn main(){std::process::exit(1);}\n', encoding='utf-8')
    first = root / 'tools' / ('rustc' + suffix)
    code, out, err = run([compiler, str(placeholder), '--edition', '2024', '-C', 'opt-level=s', '-C', 'debuginfo=0', '-o', str(first)], 'inventory-build', os.environ, 90)
    assert code == 0, err[-2000:]
    inventory = ['python3', 'node', 'go', 'java', 'ruby', 'php', 'dotnet', 'git', 'cargo', 'npm', 'uv', 'docker', 'kubectl', 'rg', 'gh', 'rust-analyzer', 'typescript-language-server', 'pyright-langserver', 'gopls', 'clangd']
    for name in inventory:
        shutil.copy2(first, root / 'tools' / (name + suffix))
    for marker, ttl, caption in [(markers[0], '5m', ''), (markers[1], '1h', ' \t\r\n\u2003')]:
        print(json.dumps({'phase': marker}), flush=True)
        case = root / marker
        case.mkdir()
        home = case / 'home'
        home.mkdir()
        workspace = case / 'workspace'
        workspace.mkdir()
        (workspace / 'sentinel.txt').write_bytes(b'owned unchanged')
        before = {p.name: sha(p) for p in workspace.iterdir()}
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_BUILTIN_SKILLS=str(root / 'empty-skills'), PATH=str(root / 'tools'), ROOK_LOG='error', ROOK_NATIVE_IMAGE_KEY='owned-native-image-key', ANTHROPIC_API_KEY='owned-native-image-key')
        code, out, err = run([str(seed), 'seed', str(case), caption], marker + '-seed', env)
        assert code == 0, err[-2000:]
        seeded = json.loads(out)
        (home / 'config.toml').write_text(f"[agent]\nmodel='fixture'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=3\nprompt_cache_ttl='{ttl}'\n[memory]\nenabled=false\n[server]\nport=0\n[models.fixture]\nmodel='claude-opus-5'\napi='anthropic'\nkey='env:ROOK_NATIVE_IMAGE_KEY'\nurl='http://127.0.0.1:{server.server_port}'\ncontext_window=131072\n", encoding='utf-8')
        client = {}
        if marker == markers[0]:
            code, out, err = run([binaries['rook']['path'], '--workspace', str(workspace), 'run', '--session', seeded['session'], marker], marker + '-cli', env, 120, workspace)
            client = {'exit_code': code, 'completed': code == 0 and b'owned image replay done' in out, 'stderr': err.decode('utf-8', errors='replace')[-2000:]}
        else:
            for label in ['stdout', 'stderr']:
                daemon_logs.append((root / 'logs' / ('daemon.' + label)).open('xb'))
            daemon = subprocess.Popen([binaries['rookd']['path'], '--workspace', str(workspace), '--port', '0'], env=env, cwd=workspace, stdin=subprocess.DEVNULL, stdout=daemon_logs[0], stderr=daemon_logs[1], creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            until = time.monotonic() + 35
            while True:
                assert daemon.poll() is None and time.monotonic() < until, 'owned daemon startup deadline'
                try:
                    base = (home / 'rookd.addr').read_text().strip()
                    with opener.open(base + '/api/health', timeout=2) as response:
                        assert response.status == 200
                    break
                except (OSError, ValueError):
                    time.sleep(0.05)
            status = case / 'client.json'
            code, out, err = run([node, str(scripts / 'client.mjs'), base, seeded['session'], marker, str(status)], marker + '-client', env, 110)
            client = json.loads(status.read_bytes())
            client['exit_code'] = code
            client['completed'] = code == 0 and client['started'] and client['done'] and (client['stopped'] == 'end_turn')
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=5)
            daemon = None
        code, out, err = run([str(seed), 'verify', str(case)], marker + '-verify', env)
        assert code == 0, err[-2000:]
        report['cases'].append({'marker': marker, 'ttl': ttl, 'session': seeded['session'], 'seed_body': json.loads(seeded['body']), 'client': client, 'stored_input_unchanged': json.loads(out)['original_saved_input_unchanged'], 'workspace_unchanged': before == {p.name: sha(p) for p in workspace.iterdir()}})
    observations = []
    for case in report['cases']:
        requests = [r for r in wire if r['marker'] == case['marker'] and (not r['checking'])]
        assert len(requests) == 1, (case['marker'], len(requests), errors)
        body = requests[0]['body']
        found = []
        for message in body['messages']:
            blocks = message.get('content')
            if message['role'] == 'user' and isinstance(blocks, list) and any((b.get('type') == 'image' for b in blocks)):
                found.append(blocks)
        assert len(found) == 1, (case['marker'], 'legacy image message not observed')
        blocks = found[0]
        images = [b for b in blocks if b['type'] == 'image']
        case['wire'] = requests[0]
        observations.append({'shape': len(blocks) == 2 and all((b['type'] == 'image' for b in blocks)), 'images': len(images) == 2 and [b['source'] for b in images] == [{'type': 'base64', 'media_type': i['mime_type'], 'data': i['data']} for i in case['seed_body']['images']], 'cache': len(images) == 2 and images[-1].get('cache_control') == ({'type': 'ephemeral'} if case['ttl'] == '5m' else {'type': 'ephemeral', 'ttl': '1h'}) and (images[0].get('cache_control') is None)})
    report['checks'] = {'local_cli_image_only_blocks': observations[0]['shape'], 'daemon_image_only_blocks': observations[1]['shape'], 'exact_image_payloads_preserved': all((r['images'] for r in observations)), 'cache_boundaries_and_ttls_preserved': all((r['cache'] for r in observations)), 'normal_turns_and_exact_owned_requests': len(wire) == 4 and all((sum((r['marker'] == m and r['checking'] for r in wire)) == 1 for m in markers)) and (not errors) and all((c['client']['completed'] for c in report['cases'])) and all((r['path'] == '/v1/messages' and r['auth_owned'] for r in wire)), 'original_saved_inputs_unchanged': all((c['stored_input_unchanged'] for c in report['cases'])), 'workspace_unchanged': all((c['workspace_unchanged'] for c in report['cases'])), 'binary_pair_unchanged': all((sha(Path(b['path'])) == b['sha256'] for b in binaries.values()))}
    assert len(report['checks']) == 8
    report['status'] = 'passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure'] = repr(error)[:2000]
    raise
finally:
    if daemon is not None:
        daemon.terminate()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=5)
    for stream in daemon_logs:
        stream.close()
    server.shutdown()
    server.server_close()
    report['model_observer_errors'] = errors
    report['owned_requests'] = wire
    with output.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps({'status': report['status'], 'checks': report.get('checks'), 'output': str(output), 'failure': report.get('failure')}), flush=True)
assert report['status'] == 'passed', report.get('checks')
