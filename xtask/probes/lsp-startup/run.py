"""Owned daemon/LSP concurrency check, without a live model or user store."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__:
    parser.error('Run without Python optimization.')
if args.output.exists() or not args.output.parent.is_dir():
    parser.error('Choose a fresh output file in an existing directory.')
binary = args.bin_dir.resolve() / ('rookd.exe' if os.name == 'nt' else 'rookd')
assert binary.is_file()
scripts = Path(__file__).resolve().parent
labels = ['NATIVE_LSP_WARM', 'NATIVE_LSP_RUST1', 'NATIVE_LSP_RUST2',
          'NATIVE_LSP_PYTHON', 'NATIVE_LSP_GO', 'NATIVE_LSP_SYMBOL']
counts, classifiers, errors, wire = {}, {}, [], []
class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *unused):
        pass
    def reply(self, value, status=200, stream=False):
        if stream:
            choice = value['choices'][0]
            delta = dict(choice['message'])
            if 'tool_calls' in delta:
                delta['tool_calls'] = [dict(index=index, **call) for index, call in enumerate(delta['tool_calls'])]
            first = dict(value, choices=[{'index': 0, 'delta': delta, 'finish_reason': None}])
            last = dict(value, choices=[{'index': 0, 'delta': {}, 'finish_reason': choice['finish_reason']}])
            payload = f'data: {json.dumps(first)}\n\ndata: {json.dumps(last)}\n\ndata: [DONE]\n\n'.encode()
        else:
            payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    def do_GET(self):
        self.reply({'data': [{'id': 'native-lsp'}]})
    def do_POST(self):
        try:
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            user = '\n'.join(str(m.get('content', '')) for m in request['messages'] if m['role'] == 'user')
            label = next(label for label in labels if label in user)
            assert len(wire) < 18, 'owned request limit exceeded'
            wire.append({'label': label, 'stream': request.get('stream'),
                         'results': [dict(id=m.get('tool_call_id'), content=str(m.get('content'))[:400])
                                     for m in request['messages'] if m['role'] == 'tool'][-4:]})
            checking = any(str(m.get('content', '')).startswith('Classify whether an assistant')
                           for m in request['messages'])
            if checking:
                assert counts.get(label) == 2
                classifiers[label] = classifiers.get(label, 0) + 1
                assert classifiers[label] == 1
                self.reply({'id': label + '-check', 'object': 'chat.completion', 'model': 'native-lsp',
                    'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': '{"action":"finish"}'},
                                 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 16, 'completion_tokens': 16}})
                return
            counts[label] = counts.get(label, 0) + 1
            assert counts[label] <= 2 and sum(counts.values()) <= 12, counts
            results = [m for m in request['messages'] if m['role'] == 'tool']
            tool = 'find_symbol' if label.endswith('SYMBOL') else 'diagnostics'
            if counts[label] == 1:
                assert tool in [entry['function']['name'] for entry in request['tools']], tool
                file = 'lib.rs' if 'RUST' in label else 'main.go' if label.endswith('GO') else 'main.py'
                parameters = {'query': 'native_symbol'} if tool == 'find_symbol' else {'path': file}
                message = {'role': 'assistant', 'content': None, 'tool_calls': [
                    {'id': label + '-call', 'type': 'function', 'function': {'name': tool, 'arguments': json.dumps(parameters)}}]}
                finish = 'tool_calls'
            else:
                expected = 'no symbol matching' if tool == 'find_symbol' else 'no diagnostics for'
                assert len(results) == 1 and expected in str(results[0]['content']), results
                message = {'role': 'assistant', 'content': 'Owned LSP check completed.'}
                finish = 'stop'
            self.reply({'id': label, 'object': 'chat.completion', 'model': 'native-lsp',
                        'choices': [{'index': 0, 'message': message, 'finish_reason': finish}],
                        'usage': {'prompt_tokens': 16, 'completion_tokens': 16}}, stream=request.get('stream', False))
        except Exception as error:
            if len(errors) < 24:
                errors.append(repr(error)[:2000])
            self.reply({'error': {'message': 'owned model fixture: ' + repr(error)[:1000]}}, 500)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
daemon, clients = None, []
temporary = tempfile.TemporaryDirectory(prefix='rook-lsp-native-')
root = Path(temporary.name)
gate = root / 'release-rust'
try:
    home, workspace = root / 'home', root / 'workspace'
    home.mkdir(); workspace.mkdir()
    (root / 'builtin-empty').mkdir()
    for name, content in [('lib.rs', 'fn main() {}\n'), ('main.py', 'x = 1\n'), ('main.go', 'package main\n')]:
        (workspace / name).write_text(content)
    config = f'''[agent]
model='native-lsp'
install_servers=false
plan_first=false
one_script=false
max_steps=3
[server]
port=0
[models.native-lsp]
model='native-lsp'
api='openai'
parallel=6
url='http://127.0.0.1:{server.server_port}/v1'
context_window=32768
'''
    for language, extension in [('rust', 'rs'), ('python', 'py'), ('go', 'go')]:
        values = [str(scripts / 'mock.py'), language, str(root / (language + '.jsonl')),
                  str(gate) if language == 'rust' else '-']
        config += '\n[[lsp]]\n' + f'language={json.dumps(language)}\ncommand={json.dumps(sys.executable)}\n'
        config += f'args={json.dumps(values)}\nextensions={json.dumps([extension])}\n'
        config += 'startup_timeout_secs=180\nrequest_timeout_secs=10\ndiagnostics_wait_ms=20\n'
    (home / 'config.toml').write_text(config, encoding='utf-8')
    env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
               ROOK_BUILTIN_SKILLS=str(root / 'builtin-empty'), TMPDIR=str(root), ROOK_LOG='error')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def until(check, label, seconds=35):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            assert not errors, errors
            value = check()
            if value:
                return value
            time.sleep(.025)
        raise AssertionError(label)
    daemon_log = (root / 'daemon.log').open('wb')
    daemon = subprocess.Popen([str(binary), '--workspace', str(workspace), '--port', '0'],
                              env=env, cwd=workspace, stdout=daemon_log, stderr=daemon_log)
    def address():
        assert daemon.poll() is None, 'owned daemon exited'
        try:
            value = (home / 'rookd.addr').read_text().strip()
            with opener.open(value + '/api/health', timeout=3) as response:
                assert response.status == 200
            return value
        except (OSError, ValueError):
            return None
    base_url = until(address, 'owned daemon startup')
    def client(label):
        output = root / (label + '.json')
        process = subprocess.Popen(['node', str(scripts / 'client.mjs'), base_url, label, str(output)],
                                   env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        item = (process, output)
        clients.append(item)
        return item
    def completed(item):
        process, output = item
        if process.poll() is None:
            return None
        status = json.loads(output.read_text())
        assert process.returncode == 0 and status['done'] and not status['error'], status
        return status
    def records(language):
        path = root / (language + '.jsonl')
        if not path.is_file():
            return []
        assert path.stat().st_size < 64 * 1024
        return [json.loads(line) for line in path.read_text().splitlines() if line]
    warm = client(labels[0]); until(lambda: completed(warm), 'Python warm chat')
    assert counts.get(labels[0]) == 2 and sum(row['method'] == 'initialize' for row in records('python')) == 1
    first = client(labels[1])
    until(lambda: any(row['method'] == 'initialize' for row in records('rust')), 'physical held Rust initialize')
    second = client(labels[2])
    until(lambda: counts.get(labels[2]) == 1, 'second Rust caller reached model')
    independent = [client(label) for label in labels[3:]]
    independent_error = None
    try:
        until(lambda: all(completed(item) for item in independent), 'ready/independent chats blocked behind Rust')
    except AssertionError as error:
        independent_error = str(error)
    rust_held = first[0].poll() is None and second[0].poll() is None
    go_before_release = any(row['method'] == 'initialize' for row in records('go'))
    symbol_before_release = any(row['method'] == 'workspace/symbol' for row in records('python'))
    gate.write_text('release')
    for item in [first, second, *independent]:
        until(lambda item=item: completed(item), 'owned chat did not finish after release')
    starts = {language: sum(row['method'] == 'initialize' for row in records(language))
              for language in ['rust', 'python', 'go']}
    report = {'checks': {'independent_chats_finish_while_rust_held': independent_error is None,
                        'both_rust_callers_stay_held': rust_held, 'go_starts_before_rust_release': go_before_release,
                        'generic_query_uses_ready_python_before_release': symbol_before_release,
                        'one_physical_process_per_language': starts == {'rust': 1, 'python': 1, 'go': 1},
                        'all_six_chats_complete': all(completed(item) for item in clients),
                        'two_model_requests_per_chat': counts == dict.fromkeys(labels, 2),
                        'one_completion_check_per_chat': classifiers == dict.fromkeys(labels, 1)},
              'binary': str(binary), 'starts': starts, 'requests': counts, 'classifiers': classifiers,
              'independent_error': independent_error,
              'scope': 'Owned daemon, scripted model and mock LSP processes; not real-server indexing, model quality or installation deduplication.'}
    with binary.open('rb') as executable:
        report['binary_sha256'] = hashlib.file_digest(executable, 'sha256').hexdigest()
    args.output.write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps(report), flush=True)
    assert all(report['checks'].values()), report
except BaseException as error:
    status = {'probe_failure': repr(error), 'requests': counts, 'classifiers': classifiers, 'model_errors': errors, 'wire': wire,
              'clients': [dict(pid=process.pid, exit=process.poll(),
                               status=json.loads(output.read_text()) if output.exists() else None)
                          for process, output in clients]}
    if 'daemon_log' in globals():
        daemon_log.flush()
        with (root / 'daemon.log').open('rb') as source:
            source.seek(0, 2)
            source.seek(max(0, source.tell() - 8192))
            status['daemon_log_tail'] = source.read(8192).decode(errors='replace')
    print(json.dumps(status), flush=True)
    if not args.output.exists():
        args.output.write_text(json.dumps(status, indent=2), encoding='utf-8')
    raise
finally:
    gate.write_text('release')
    for process, _ in clients:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=5)
    if daemon is not None and daemon.poll() is None:
        daemon.terminate()
        try:
            daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:
            daemon.kill(); daemon.wait(timeout=5)
    if 'daemon_log' in globals():
        daemon_log.close()
    server.shutdown(); server.server_close()
    temporary.cleanup()
