"""Owned ACP null/default payload checks against an actual CLI and local provider."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
binary = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
if not __debug__ or not binary.is_file() or args.output.exists() or not args.output.parent.is_dir():
    parser.error('Run without optimization, with an existing CLI and a fresh report path.')
with binary.open('rb') as executable:
    binary_hash = hashlib.file_digest(executable, 'sha256').hexdigest()
counts = {}
errors = []
request_kinds = {}
retrying = set()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *unused):
        pass

    def do_POST(self):
        try:
            self.connection.settimeout(10)
            case = self.path.split('/')[1]
            assert case in counts
            phase = case + '-retry' if case in retrying else case
            size = int(self.headers['Content-Length'])
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            checking = any('You are checking a claim somebody else made.' in str(message.get('content', ''))
                           for message in request['messages'])
            counts[phase] += 1
            assert counts[phase] <= 4
            request_kinds[phase].append('classifier' if not request.get('stream') else 'verifier' if checking else 'main')
            if request.get('stream'):
                if counts[phase] == 1:
                    tool = 'run_command' if case == 'null-terminal' else 'read_file' if case == 'null-read' else 'write_file'
                    arguments = {'command': 'owned-not-executed'} if tool == 'run_command' else {'path': 'notes.txt'}
                    if tool == 'write_file':
                        arguments['content'] = 'owned editor update\n'
                    delta = {'role': 'assistant', 'tool_calls': [{'index': 0, 'id': 'owned-call',
                        'type': 'function', 'function': {'name': tool, 'arguments': json.dumps(arguments)}}]}
                    finish = 'tool_calls'
                else:
                    text = 'VERDICT: unproven' if checking else 'Owned exchange complete.'
                    delta = {'role': 'assistant', 'content': text}
                    finish = 'stop'
                frame = {'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}],
                         'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}
                body = ('data: ' + json.dumps(frame) + '\n\ndata: [DONE]\n\n').encode()
                content_type = 'text/event-stream'
            else:
                body = json.dumps({'choices': [{'message': {'role': 'assistant',
                    'content': '{"action":"finish"}'}, 'finish_reason': 'stop'}],
                    'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}).encode()
                content_type = 'application/json'
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:
            if len(errors) < 8:
                errors.append(str(error)[:1000])
            self.close_connection = True


def run_case(root, case, port):
    directory = root / case
    directory.mkdir()
    home = directory / 'home'
    home.mkdir()
    workspace = directory / 'workspace'
    workspace.mkdir()
    empty = directory / 'empty'
    empty.mkdir()
    original = 'owned disk contents\n'
    (workspace / 'notes.txt').write_text(original, encoding='utf-8')
    (home / 'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
[sandbox]
stance='autonomous'
[models.fixture]
api='openai'
model='owned-fixture'
url='http://127.0.0.1:{port}/{case}/v1'
''', encoding='utf-8')
    env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
               ROOK_BUILTIN_SKILLS=str(empty), ROOK_LOG='error', PATH=str(empty))
    inbox = queue.Queue(maxsize=64)
    with (directory / 'stderr').open('wb') as log:
        process = subprocess.Popen([str(binary), '--workspace', str(workspace), 'acp'],
            env=env, cwd=workspace, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))

        def read_frames():
            try:
                for _ in range(256):
                    line = process.stdout.readline(1024 * 1024 + 1)
                    assert line and len(line) <= 1024 * 1024
                    inbox.put(json.loads(line), timeout=5)
            except BaseException as error:
                try:
                    inbox.put(error, timeout=5)
                except queue.Full:
                    pass

        reader = threading.Thread(target=read_frames, daemon=True)
        reader.start()
        deadline = time.monotonic() + 120

        def send(frame):
            process.stdin.write((json.dumps(frame) + '\n').encode())
            process.stdin.flush()

        def receive():
            remaining = deadline - time.monotonic()
            assert remaining > 0, 'owned ACP exchange deadline'
            frame = inbox.get(timeout=min(60, remaining))
            assert isinstance(frame, dict), str(frame)
            return frame

        def call(identifier, method, params=...):
            frame = {'jsonrpc': '2.0', 'id': identifier, 'method': method}
            if params is not ...:
                frame['params'] = params
            send(frame)
            for _ in range(64):
                frame = receive()
                if frame.get('id') == identifier:
                    return frame
            raise AssertionError('bounded ACP response frame count')

        try:
            initialized = call(1, 'initialize', {'protocolVersion': 1,
                'clientCapabilities': {'fs': {'readTextFile': True, 'writeTextFile': True},
                                       'terminal': case == 'null-terminal'}})
            assert initialized['result']['protocolVersion'] == 1
            opened = call(2, 'session/new', {'cwd': str(workspace)})
            session = opened['result']['sessionId']
            listing = {name: call(i, 'session/list', value) for i, (name, value) in enumerate(
                [('object', {}), ('null', None), ('omitted', ...), ('invalid', False)], start=10)}
            send({'jsonrpc': '2.0', 'id': 3, 'method': 'session/prompt',
                  'params': {'sessionId': session, 'prompt': [{'type': 'text', 'text': 'Perform the owned file operation.'}]}})
            buffer = original
            methods = []
            statuses = []
            for _ in range(128):
                frame = receive()
                method = frame.get('method')
                if method in ['fs/read_text_file', 'fs/write_text_file'] or (method and method.startswith('terminal/')):
                    assert frame['params']['sessionId'] == session
                    if method.startswith('fs/'):
                        assert Path(frame['params']['path']).samefile(workspace / 'notes.txt')
                    methods.append(method)
                    assert len(methods) <= 12
                    reply = {'jsonrpc': '2.0', 'id': frame['id']}
                    if method == 'fs/write_text_file':
                        buffer = frame['params']['content']
                        if case == 'error-write':
                            reply['error'] = {'code': -32000, 'message': 'owned write error'}
                        elif case == 'invalid-write':
                            reply['result'] = False
                        elif case != 'missing-write':
                            reply['result'] = {} if case == 'object-write' else None
                    elif method == 'fs/read_text_file':
                        reply['result'] = None if case == 'null-read' else {'content': buffer}
                    elif method == 'terminal/create':
                        reply['result'] = {'terminalId': 'owned-terminal'}
                    elif method == 'terminal/output':
                        reply['result'] = {'output': 'owned terminal output\n', 'truncated': False,
                                           'exitStatus': {'exitCode': 0}}
                    else:
                        reply['result'] = None
                    send(reply)
                elif method == 'session/update':
                    update = frame['params']['update']
                    if update.get('sessionUpdate') == 'tool_call_update' and update.get('status') in ['completed', 'failed']:
                        statuses.append(update['status'])
                elif frame.get('id') == 3:
                    assert 'result' in frame, frame
                    break
            else:
                raise AssertionError('bounded turn frame count')
            report = {'methods': methods, 'statuses': statuses, 'buffer': buffer,
                    'disk_unchanged': (workspace / 'notes.txt').read_text(encoding='utf-8') == original,
                    'list': {name: 'result' in frame and any(item['sessionId'] == session for item in frame['result']['sessions'])
                             for name, frame in listing.items() if name != 'invalid'},
                    'invalid_list_refused': listing['invalid'].get('error', {}).get('code') == -32602}
        except BaseException:
            log.flush()
            with (directory / 'stderr').open('rb') as diagnostic:
                diagnostic.seek(max(0, diagnostic.seek(0, 2) - 4096))
                print(diagnostic.read(4096).decode('utf-8', errors='replace'), flush=True)
            raise
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            reader.join(timeout=2)
            process.stdout.close()
            assert process.returncode == 0, f'owned ACP CLI exit {process.returncode}'
    inspection = subprocess.run([str(binary), '--workspace', str(workspace), 'session', 'recovery', session],
        env=env, cwd=workspace, capture_output=True, timeout=30,
        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    assert inspection.returncode == 0, inspection.stderr[-4096:]
    assert len(inspection.stdout) <= 128 * 1024
    executions = json.loads(inspection.stdout)
    assert isinstance(executions, list) and len(executions) <= 8
    unknown = {operation['id']: operation for execution in executions for operation in execution['unknown']}
    report['unknown'] = [{'id': operation['id'], 'tool': operation['tool'], 'may_have_effects': operation['may_have_effects']}
                         for operation in unknown.values()]
    report['recovery_pending'] = any(execution['pending'] or execution['background'] for execution in executions)
    report['retry'] = None
    if case in ['missing-write', 'error-write', 'invalid-write'] and unknown:
        phase = case + '-retry'
        counts[phase] = 0
        request_kinds[phase] = []
        retrying.add(case)
        retried = subprocess.run([str(binary), '--workspace', str(workspace), 'run', '--session', session,
            'Retry the owned file write.'], env=env, cwd=workspace, capture_output=True, timeout=30,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        text = (retried.stdout + retried.stderr).decode('utf-8', errors='replace')
        reviewed = subprocess.run([str(binary), '--workspace', str(workspace), 'session', 'recovery', session],
            env=env, cwd=workspace, capture_output=True, timeout=30,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        assert reviewed.returncode == 0 and len(reviewed.stdout) <= 128 * 1024
        after = {operation['id'] for execution in json.loads(reviewed.stdout) for operation in execution['unknown']}
        report['retry'] = {'exit': retried.returncode, 'tail': text[-2400:],
                          'provider_requests': counts[phase], 'unknown_preserved': set(unknown) == after,
                          'disk_unchanged': (workspace / 'notes.txt').read_text(encoding='utf-8') == original}
    return report


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
worker = threading.Thread(target=server.serve_forever, daemon=True)
worker.start()
try:
    with tempfile.TemporaryDirectory(prefix='rook-acp-null-') as temporary:
        cases = {}
        for case in ['object-write', 'null-write', 'missing-write', 'error-write', 'invalid-write', 'null-read', 'null-terminal']:
            counts[case] = 0
            request_kinds[case] = []
            cases[case] = run_case(Path(temporary), case, server.server_port)
        checks = {
            'object_write_acknowledged': cases['object-write']['statuses'] == ['completed'],
            'null_write_acknowledged': cases['null-write']['statuses'] == ['completed'],
            'missing_result_refused': cases['missing-write']['statuses'] == ['failed'],
            'explicit_error_refused': cases['error-write']['statuses'] == ['failed'],
            'invalid_scalar_acknowledgement_refused': cases['invalid-write']['statuses'] == ['failed'],
            'required_read_content_not_defaulted': cases['null-read']['statuses'] == ['failed'],
            'null_terminal_cleanup_retires_once': cases['null-terminal']['statuses'] == ['completed']
                and cases['null-terminal']['methods'] == ['terminal/create', 'terminal/wait_for_exit', 'terminal/output', 'terminal/release'],
            'null_and_omitted_list_are_default_requests': all(all(case['list'].values()) for case in cases.values()),
            'invalid_nonnull_list_refused': all(case['invalid_list_refused'] for case in cases.values()),
            'uncertain_write_retained_after_connection_close': all(
                len(case['unknown']) == 1 and case['unknown'][0]['tool'] == 'write_file'
                and case['unknown'][0]['may_have_effects'] and not case['recovery_pending']
                for name, case in cases.items() if name in ['missing-write', 'error-write', 'invalid-write'])
                and all(not case['unknown'] for name, case in cases.items()
                        if name not in ['missing-write', 'error-write', 'invalid-write']),
            'fresh_process_retry_keeps_writes_blocked': all(case['retry']
                and 0 <= case['retry']['provider_requests'] <= 4 and case['retry']['unknown_preserved']
                and case['retry']['disk_unchanged']
                and 'recovery' in case['retry']['tail'].lower()
                for name, case in cases.items() if name in ['missing-write', 'error-write', 'invalid-write']),
            'editor_buffers_used_without_disk_fallback': all(case['disk_unchanged'] for case in cases.values())
                and all(case['buffer'] == 'owned editor update\n' for name, case in cases.items() if name.endswith('-write'))
                and all('fs/write_text_file' in case['methods'] for name, case in cases.items() if name.endswith('-write')),
            'bounded_local_provider_requests': not errors and all(
                value == ['main', 'main', 'verifier', 'classifier'] if not name.endswith('-retry')
                else len(value) <= 4 and (not value or value[0] == 'main')
                for name, value in request_kinds.items()),
        }
finally:
    server.shutdown()
    server.server_close()
    worker.join(timeout=2)
with binary.open('rb') as executable:
    assert binary_hash == hashlib.file_digest(executable, 'sha256').hexdigest()
report = {'binary': str(binary), 'binary_sha256': binary_hash, 'checks': checks, 'cases': cases,
          'provider_requests': counts, 'errors': errors, 'request_kinds': request_kinds}
with args.output.open('x', encoding='utf-8') as output:
    json.dump(report, output, indent=2)
print(json.dumps({'checks': checks, 'provider_requests': counts}), flush=True)
assert all(checks.values()), checks
