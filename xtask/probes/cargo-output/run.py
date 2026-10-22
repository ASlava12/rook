"""Actual CLI/daemon Cargo capture, with an owned compiler-shaped executable."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
args = parser.parse_args()
assert __debug__
output = args.output.resolve()
assert not output.exists() and output.parent.is_dir()
scripts = Path(__file__).resolve().parent
bins = args.bin_dir.resolve()
compiler, node = shutil.which('rustc'), shutil.which('node')
assert compiler and node
suffix = '.exe' if os.name == 'nt' else ''
root = Path(tempfile.mkdtemp(prefix='rook-cargo-output-', dir=args.tmp_root)).resolve()
for name in ['tools', 'logs', 'empty-skills']:
    (root / name).mkdir()

def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

binaries = {name: {'path': str(bins / (name + suffix)), 'sha256': sha(bins / (name + suffix))}
            for name in ['rook', 'rookd']}
inputs = {name: sha(scripts / name) for name in ['run.py', 'helper.rs', 'client.mjs']}
report = {'status': 'incomplete', 'owned_directory': str(root), 'binaries': binaries,
          'source_inputs': inputs, 'cases': []}
expected_original = (
    ''.join(f'   Compiling owned_{n} v1.0.0\n' for n in range(4096))
    + 'error[E0502]: OWNED_MIDDLE_DIAGNOSTIC\n'
    + ' --> src/main.rs:7:11\n  |\n7 | exact_source();\n  | ^^^^^^^^^^^^ OWNED_SOURCE_SPAN\n'
    + 'warning: OWNED_WARNING_REMAINS\n --> src/lib.rs:3:1\n'
    + ''.join(f'test owned_test_{n} ... ok\n' for n in range(4096))
    + 'test result: FAILED. 0 passed; 1 failed; 0 ignored\n'
    + 'error: could not compile owned-fixture due to previous error\n'
).encode('utf-8')
assert len(expected_original) == 239726
report['expected_original_sha256'] = hashlib.sha256(expected_original).hexdigest()

def spawn(command, name, env, cwd):
    child = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    captures = {'stdout': bytearray(), 'stderr': bytearray()}
    overflow = threading.Event()
    def drain(label, stream):
        while chunk := stream.read1(4096):
            room = 65536 - len(captures[label])
            captures[label].extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if child.poll() is None:
                    child.kill()
    readers = [threading.Thread(target=drain, args=(label, stream), daemon=True)
               for label, stream in [('stdout', child.stdout), ('stderr', child.stderr)]]
    for reader in readers:
        reader.start()
    return child, (name, captures, overflow, readers)

def finish(child, capture):
    name, captures, overflow, readers = capture
    if child.poll() is None:
        child.kill()
        child.wait(timeout=10)
    for reader in readers:
        reader.join(timeout=10)
    assert not any(reader.is_alive() for reader in readers)
    child.stdout.close()
    child.stderr.close()
    for label, data in captures.items():
        (root / 'logs' / (name + '.' + label)).write_bytes(data)
    assert not overflow.is_set(), 'owned child exceeded64KiB per stream'
    return child.returncode, bytes(captures['stdout']), bytes(captures['stderr'])

def run(command, name, env, cwd, timeout=120):
    child, capture = spawn(command, name, env, cwd)
    try:
        child.wait(timeout=timeout)
    finally:
        result = finish(child, capture)
    return result

records = {name: {'main': 0, 'auxiliary': 0, 'auxiliary_kinds': [], 'catalog_mode': False, 'tools': []}
           for name in ['cli', 'daemon']}
errors, paths = [], []
lock = threading.Lock()
commands = {}

def tool_content(content):
    # Inspect the ordinary data envelope rather than escaped JSON text.
    try:
        value = json.loads(content)
    except (ValueError, TypeError):
        return content
    source = value.get('rook_source') if isinstance(value, dict) else None
    if isinstance(source, dict) and isinstance(source.get('content'), str):
        return source['content']
    return content

class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)
    def log_message(self, *unused):
        pass
    def send(self, value, stream=False):
        data = (('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n') if stream else json.dumps(value)).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        with lock:
            if len(errors) < 8:
                errors.append('unexpected model GET ' + self.path[:200])
        self.send_error(500)
    def do_POST(self):
        try:
            size = int(self.headers.get('Content-Length', '0'))
            assert 0 < size <= 2 * 1024 * 1024 and len(self.path) <= 512
            name = self.path.split('/')[1]
            assert name in records and self.path == '/' + name + '/v1/chat/completions'
            request = json.loads(self.rfile.read(size))
            classifier = any('Classify whether an assistant' in str(m.get('content', '')) for m in request['messages'])
            verifier = any('checking a claim somebody else made' in str(m.get('content', '')) for m in request['messages'])
            checking = classifier or verifier
            with lock:
                assert len(paths) < 20
                paths.append(self.path)
                record = records[name]
                if checking:
                    record['auxiliary'] += 1
                    record['auxiliary_kinds'].append('classifier' if classifier else 'verifier')
                    assert record['auxiliary'] <= 2
                    index = -1
                else:
                    record['main'] += 1
                    index = record['main'] - 1
                    assert index < 3
                    if index == 0:
                        spec = next(t['function'] for t in request['tools'] if t['function']['name'] == 'run_command')
                        # Default catalogs offer name/type stubs, with full enums
                        # available through tool_schema rather than every request.
                        record['catalog_mode'] = spec['parameters']['properties'].get('output', {}).get('type') == 'string'
                    if index == 2:
                        record['tools'] = [{'id': m.get('tool_call_id'), 'content': tool_content(m['content'])}
                                           for m in request['messages'] if m.get('role') == 'tool']
            text = '{"action":"finish"}' if classifier else 'VERDICT: unproven' if verifier else 'OWNED_CARGO_CAPTURE_DONE'
            if index in [0, 1]:
                mode = 'auto' if index == 0 else 'raw'
                delta = {'role': 'assistant', 'tool_calls': [{'index': 0, 'id': 'owned-' + mode,
                    'type': 'function', 'function': {'name': 'run_command',
                    'arguments': json.dumps({'command': commands[mode], 'output': mode})}}]}
                ending = 'tool_calls'
            else:
                delta, ending = {'role': 'assistant', 'content': text}, 'stop'
            if request.get('stream'):
                self.send({'model': 'owned-cargo', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': ending}],
                           'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}, True)
            else:
                self.send({'model': 'owned-cargo', 'choices': [{'message': delta, 'finish_reason': ending}],
                           'usage': {'prompt_tokens': 1, 'completion_tokens': 1}})
        except BaseException as error:
            with lock:
                if len(errors) < 8:
                    errors.append(type(error).__name__ + ': ' + str(error)[:1000])
            self.close_connection = True

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
daemon = capture = None
try:
    helper = root / 'tools' / ('cargo' + suffix)
    version_home = root / 'version-home'
    version_home.mkdir()
    env = dict(os.environ, PYTHONIOENCODING='utf-8', ROOK_HOME=str(version_home), ROOK_CONFIG_DIR=str(version_home))
    for name, binary in binaries.items():
        code, stdout, stderr = run([binary['path'], '--version'], 'version-' + name, env, root, 30)
        assert code == 0, stderr[-2000:]
        binary['version'] = stdout.decode('utf-8').strip()
    version = binaries['rook']['version'].removeprefix('rook ')
    assert re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?', version)
    assert binaries['rookd']['version'] == 'rookd ' + version
    code, stdout, stderr = run([compiler, '--edition', '2024', '-C', 'debuginfo=0', str(scripts / 'helper.rs'), '-o', str(helper)], 'helper-build', env, root)
    assert code == 0, stderr[-2000:]
    helper_sha = sha(helper)
    for program in ['rustc', 'python3', 'node', 'go', 'java', 'ruby', 'php', 'dotnet', 'git', 'npm', 'uv', 'docker', 'kubectl', 'rg', 'gh']:
        shutil.copy2(helper, root / 'tools' / (program + suffix))
    commands.update(auto='"' + str(helper) + '" check', raw='"' + str(helper) + '" check --owned-raw')
    system_paths = str(Path(os.environ['SystemRoot']) / 'System32') if os.name == 'nt' else '/usr/bin:/bin'
    for name in ['cli', 'daemon']:
        print(json.dumps({'case': name}), flush=True)
        case = root / name
        home, workspace = case / 'home', case / 'workspace'
        home.mkdir(parents=True)
        workspace.mkdir()
        (workspace / 'sentinel.txt').write_bytes(b'Owned sentinel unchanged')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
                   ROOK_BUILTIN_SKILLS=str(root / 'empty-skills'), ROOK_LOG='error',
                   ROOK_CARGO_OUTPUT_KEY='owned-no-external-key', PATH=str(root / 'tools') + os.pathsep + system_paths)
        (home / 'config.toml').write_text(f"[agent]\nmodel='fixture'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=4\n[memory]\nenabled=false\n[sandbox]\nstance='autonomous'\nisolate='off'\nmax_output_bytes=2048\nmax_spill_bytes=1048576\n[server]\nport=0\n[models.fixture]\napi='openai'\nmodel='owned-cargo'\nkey='env:ROOK_CARGO_OUTPUT_KEY'\ncontext_window=131072\nurl='http://127.0.0.1:{server.server_port}/{name}/v1'\n", encoding='utf-8')
        marker = 'OWNED_CARGO_' + name.upper()
        if name == 'cli':
            code, stdout, stderr = run([binaries['rook']['path'], '--workspace', str(workspace), 'run', marker], name, env, workspace)
            client = {'exit_code': code, 'completed': code == 0 and b'OWNED_CARGO_CAPTURE_DONE' in stdout}
        else:
            daemon, capture = spawn([binaries['rookd']['path'], '--workspace', str(workspace), '--port', '0'], 'daemon', env, workspace)
            deadline = time.monotonic() + 35
            while True:
                assert daemon.poll() is None and time.monotonic() < deadline, 'owned daemon startup deadline'
                try:
                    base = (home / 'rookd.addr').read_text(encoding='utf-8').strip()
                    with opener.open(base + '/api/health', timeout=2) as response:
                        assert response.status == 200
                    break
                except (OSError, ValueError):
                    time.sleep(0.05)
            status = case / 'client.json'
            code, stdout, stderr = run([node, str(scripts / 'client.mjs'), base, marker, str(status)], 'websocket', env, workspace)
            client = json.loads(status.read_bytes())
            client.update(exit_code=code, completed=code == 0 and client['done'] and client['stopped'] == 'end_turn')
            daemon.terminate()
            daemon.wait(timeout=10)
            finish(daemon, capture)
            daemon = capture = None
        with lock:
            record = json.loads(json.dumps(records[name]))
        tool_results = {r['id']: r['content'] for r in record['tools']}
        raw = []
        for result in record['tools']:
            match = re.search(r'\[whole output: (.+?) \((\d+) bytes', result['content'])
            if match:
                path = Path(match[1]).resolve()
                assert path.is_relative_to(home.resolve()) and path.is_file()
                with path.open('rb') as stream:
                    data = stream.read(1024 * 1024 + 1)
                assert len(data) <= 1024 * 1024
                raw.append({'path': str(path), 'size': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
                            'exact_original': data == expected_original and len(data) == int(match[2]),
                            'has_middle': b'OWNED_MIDDLE_DIAGNOSTIC' in data,
                            'has_all_progress': sum(line.startswith(b'   Compiling owned_') for line in data.splitlines()) == 4096,
                            'has_all_tests': sum(line.startswith(b'test owned_test_') for line in data.splitlines()) == 4096})
        files = {p.relative_to(workspace).as_posix(): p.read_bytes() for p in workspace.rglob('*') if p.is_file()}
        workspace_ok = files == {'sentinel.txt': b'Owned sentinel unchanged', 'invoked.txt': b'check\ncheck --owned-raw\n'} and not any(p.is_dir() for p in workspace.rglob('*'))
        report['cases'].append({'name': name, 'client': client, 'record': record, 'tool_results': tool_results,
                                'captures': raw, 'workspace_ok': workspace_ok,
                                'physical_invocations': files.get('invoked.txt', b'').decode('utf-8', errors='replace')})
    cases = {c['name']: c for c in report['cases']}
    def automatic(name):
        case = cases[name]
        body = case['tool_results'].get('owned-auto', '')
        return case['client']['completed'] and 'OWNED_MIDDLE_DIAGNOSTIC' in body and 'Cargo output condensed:' in body and 'Compiling owned_' not in body and 'test owned_test_' not in body
    all_captures = [r for c in report['cases'] for r in c['captures']]
    report['checks'] = {
        'cli_middle_diagnostics_preserved': automatic('cli'),
        'daemon_middle_diagnostics_preserved': automatic('daemon'),
        'raw_override_keeps_progress_and_tests': all('Compiling owned_0' in c['tool_results'].get('owned-raw', '') and 'test owned_test_4095 ... ok' in c['tool_results'].get('owned-raw', '') for c in report['cases']),
        'original_capture_bytes_retained': len(all_captures) == 4 and len({r['path'] for r in all_captures}) == 4 and all(r['exact_original'] and r['has_middle'] and r['has_all_progress'] and r['has_all_tests'] for r in all_captures),
        'actual_failed_exit_codes_preserved': all(len(c['tool_results']) == 2 and all('exit 101' in r for r in c['tool_results'].values()) for c in report['cases']),
        'output_mode_present_in_model_catalog': all(c['record']['catalog_mode'] for c in report['cases']),
        'exact_two_owned_command_invocations': all(c['physical_invocations'] == 'check\ncheck --owned-raw\n' for c in report['cases']),
        'warnings_spans_and_test_summary_preserved': all(all(text in c['tool_results'].get('owned-auto', '') for text in ['OWNED_WARNING_REMAINS', 'OWNED_SOURCE_SPAN', 'test result: FAILED.']) for c in report['cases']),
        'bounded_owned_requests_and_captures': not errors and len(paths) == 10 and all(c['record']['main'] == 3 and c['record']['auxiliary'] == 2 and sorted(c['record']['auxiliary_kinds']) == ['classifier', 'verifier'] for c in report['cases']),
        'only_expected_owned_workspace_effects': all(c['workspace_ok'] for c in report['cases']),
        'binary_pair_unchanged': all(sha(b['path']) == b['sha256'] for b in binaries.values()),
        'helper_and_source_unchanged': sha(helper) == helper_sha and all(sha(scripts / p) == h for p, h in inputs.items()),
    }
    assert len(report['checks']) == 12
    report['status'] = 'passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure'] = type(error).__name__ + ': ' + str(error)[:2000]
    raise
finally:
    if daemon is not None:
        finish(daemon, capture)
    server.shutdown()
    server.server_close()
    report['owned_request_paths'], report['observer_errors'] = paths, errors
    with output.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps({'status': report['status'], 'checks': report.get('checks'), 'failure': report.get('failure'), 'output': str(output)}), flush=True)
assert report['status'] == 'passed', report.get('checks')
