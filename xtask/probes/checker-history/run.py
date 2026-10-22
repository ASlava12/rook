"""Exercise bounded checker acquisition and old evidence recall with an actual CLI."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__ or args.output.exists() or not args.output.parent.is_dir():
    parser.error('Unoptimized Python and a fresh output path are required.')
binary = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
assert binary.is_file()


def fingerprint():
    with binary.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


digest = fingerprint()
steps = 80
seen = {'main': 0, 'checker': 0, 'auxiliary': 0, 'coverage': None,
        'old_proof_recalled': False, 'initial_excerpt_has_old_proof': False,
        'suggested_recall': None, 'max_request_bytes': 0}
errors = []
lock = threading.Lock()


def coverage_in(value, depth=0):
    """Unwrap the harness's nested quoted records, without retaining requests."""
    if depth > 12:
        return None
    if isinstance(value, dict):
        source = value.get('rook_source', {})
        if isinstance(source, dict) and source.get('kind') == 'history_coverage':
            return json.loads(source['content'])
        children = value.values()
    elif isinstance(value, list):
        children = value
    elif isinstance(value, str):
        hint = re.search(r'session_recall (\{"session":"[^"]+","around":\d+\})', value)
        if hint and seen['suggested_recall'] is None:
            seen['suggested_recall'] = json.loads(hint[1])
        children = []
        start = 0
        for _ in range(256):
            start = value.find('{"rook_source":', start)
            if start < 0:
                break
            try:
                child, consumed = json.JSONDecoder().raw_decode(value[start:])
                children.append(child)
                start += consumed
            except ValueError:
                start += 1
    else:
        return None
    for child in children:
        result = coverage_in(child, depth + 1)
        if result is not None:
            return result
    return None


class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, *unused):
        pass

    def send(self, value, stream=False):
        body = (('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n') if stream
                else json.dumps(value)).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.send({'data': [{'id': 'owned-history', 'context_length': 1048576}]})

    def do_POST(self):
        try:
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            messages = request['messages']
            checking = any('checking a claim somebody else made' in str(m.get('content', ''))
                           for m in messages)
            stream = bool(request.get('stream'))
            with lock:
                assert seen['main'] + seen['checker'] + seen['auxiliary'] < steps + 12
                seen['max_request_bytes'] = max(seen['max_request_bytes'], size)
                category = 'checker' if checking else 'main' if stream else 'auxiliary'
                seen[category] += 1
                index = seen[category]
            call = None
            if checking:
                coverage = coverage_in(messages)
                if index == 1:
                    seen['initial_excerpt_has_old_proof'] = 'OWNED_OLD_PROOF' in str(messages)
                if coverage and seen['coverage'] is None:
                    seen['coverage'] = coverage
                if index == 1 and coverage and seen['suggested_recall']:
                    call = ('session_recall', seen['suggested_recall'])
                if index > 1:
                    seen['old_proof_recalled'] = any(m.get('role') == 'tool'
                        and 'OWNED_OLD_PROOF' in str(m.get('content', '')) for m in messages)
                text = 'VERDICT: unproven'
            elif stream:
                if index <= steps:
                    call = ('tool_status', {'query': f'owned_history_{index:03}'})
                text = 'OWNED_OLD_PROOF' if index == 1 else 'OWNED_HISTORY_DONE' if index > steps else ''
            else:
                text = '{"action":"finish"}'
            message = {'role': 'assistant', 'content': text}
            if call:
                message['tool_calls'] = [{'index': 0, 'id': f'{category}-{index}', 'type': 'function',
                    'function': {'name': call[0], 'arguments': json.dumps(call[1])}}]
            self.send({'model': 'owned-history', 'choices': [{'index': 0,
                'delta' if stream else 'message': message,
                'finish_reason': 'tool_calls' if call else 'stop'}],
                'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}, stream)
        except Exception as error:
            with lock:
                if len(errors) < 8:
                    errors.append(repr(error)[:1000])
            self.close_connection = True


def invoke(workspace, env):
    process = subprocess.Popen([str(binary), '--workspace', str(workspace), '--json', 'run',
        'Inspect the owned tool catalog, then return OWNED_HISTORY_DONE. Do not change files.'],
        env=env, cwd=workspace, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    output = bytearray()
    overflow = threading.Event()

    def drain():
        while chunk := process.stdout.read1(4096):
            room = 1024 * 1024 - len(output)
            output.extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if process.poll() is None:
                    process.kill()

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        code = process.wait(timeout=180)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        reader.join(timeout=10)
        process.stdout.close()
    assert not reader.is_alive(), 'owned CLI output reader did not retire'
    return code, output.decode('utf-8', errors='replace'), not overflow.is_set()


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='rook-history-native-') as directory:
        root = Path(directory)
        home = root / 'home'; home.mkdir()
        workspace = root / 'workspace'; workspace.mkdir()
        empty = root / 'empty'; empty.mkdir()
        sentinel = workspace / 'sentinel'
        sentinel.write_bytes(b'owned unchanged')
        (home / 'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=100
[sandbox]
stance='autonomous'
[models.fixture]
api='openai'
model='owned-history'
context_window=1048576
url='http://127.0.0.1:{server.server_port}/v1'
''', encoding='utf-8')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
            ROOK_BUILTIN_SKILLS=str(empty), ROOK_LOG='error', PATH=str(empty))
        code, text, bounded = invoke(workspace, env)
        coverage = seen['coverage'] or {}
        checks = {
            'actual_cli_finished_owned_stage': code == 0 and 'OWNED_HISTORY_DONE' in text,
            'fixture_exceeded_retention_limit': seen['main'] == steps + 1 and steps * 2 > 128,
            'checker_received_acquisition_coverage': coverage.get('scope') == 'saved_stage_excerpt',
            'retention_bound_reached': coverage.get('retained_entries') == 128,
            'body_and_metadata_bounds': coverage.get('body_reads', 129) <= 128
                and coverage.get('body_bytes', 256001) <= 256000
                and coverage.get('metadata_steps', 1025) <= 1024,
            'older_range_explicit': coverage.get('unread_before_exclusive') is not None,
            'old_proof_readable_by_checker': seen['old_proof_recalled']
                and not seen['initial_excerpt_has_old_proof']
                and seen['suggested_recall'] == {'session': coverage.get('session'), 'around': coverage.get('from')},
            'owned_workspace_unchanged': sentinel.read_bytes() == b'owned unchanged'
                and set(workspace.iterdir()) == {sentinel},
            'bounded_capture_and_local_requests': bounded and not errors,
            'binary_unchanged': fingerprint() == digest,
        }
    report = {'binary_sha256': digest, 'checks': checks, 'requests': seen,
              'exit': code, 'output_tail': text[-4000:], 'errors': errors,
              'scope': 'Owned CLI and local scripted provider; no real model or latency/heap claim.'}
    with args.output.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps({'binary_sha256': digest, 'checks': checks}), flush=True)
    assert all(checks.values()), checks
finally:
    server.shutdown()
    server.server_close()
