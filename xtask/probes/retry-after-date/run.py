"""Check Retry-After dates with an actual CLI and owned local API."""
import argparse
import hashlib
import email.utils
import time
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
if not __debug__ or args.output.exists() or not args.output.parent.is_dir():
    parser.error('Unoptimized Python and a fresh output path are required.')
binary = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
assert binary.is_file()
def fingerprint():
    with binary.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


digest = fingerprint()
cases = ['date', 'seconds', 'past', 'too-distant', 'invalid']
seen = {name: {'main_caps': [], 'main_wall': [], 'main_mono': [], 'auxiliary': 0} for name in cases}
errors = []
lock = threading.Lock()


class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, *unused):
        pass

    def send(self, status, value, stream=False, retry_after=None):
        body = (('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n') if stream
                else json.dumps(value)).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        if retry_after is not None:
            self.send_header('Retry-After', retry_after)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.send(200, {'data': [{'id': 'owned-credit', 'context_length': 65536}]})

    def do_POST(self):
        try:
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            name = self.path.split('/')[1]
            assert name in cases
            checking = any('checking a claim somebody else made' in str(m.get('content', ''))
                           for m in request['messages'])
            main = request.get('stream') and not checking
            with lock:
                record = seen[name]
                assert len(record['main_caps']) + record['auxiliary'] < 6
                if main:
                    record['main_caps'].append(request['max_tokens'])
                    record['main_wall'].append(time.time())
                    record['main_mono'].append(time.monotonic())
                    index = len(record['main_caps']) - 1
                else:
                    record['auxiliary'] += 1
                    index = -1
            if main and index == 0:
                if name == 'seconds':
                    header = '2'
                    record['not_before_mono'] = time.monotonic() + 2
                elif name == 'invalid':
                    header = 'tomorrow'
                else:
                    delta = {'date': 4, 'past': -30, 'too-distant': 3600}[name]
                    header = email.utils.formatdate(time.time() + delta, usegmt=True)
                    if name == 'date':
                        record['not_before_wall'] = email.utils.parsedate_to_datetime(header).timestamp()
                record['retry_after'] = header
                self.send(429, {'error': {'message': 'owned retry refusal'}}, retry_after=header)
                return
            text = 'VERDICT: unproven' if checking else 'OWNED_RETRY_OK'
            if request.get('stream'):
                self.send(200, {'model': 'owned-credit', 'choices': [{'index': 0,
                    'delta': {'role': 'assistant', 'content': text}, 'finish_reason': 'stop'}],
                    'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}, True)
            else:
                self.send(200, {'model': 'owned-credit', 'choices': [{'message': {
                    'role': 'assistant', 'content': '{"action":"finish"}'}, 'finish_reason': 'stop'}],
                    'usage': {'prompt_tokens': 1, 'completion_tokens': 1}})
        except Exception as error:
            with lock:
                if len(errors) < 8:
                    errors.append(repr(error)[:1000])
            self.close_connection = True


def invoke(workspace, env):
    process = subprocess.Popen([str(binary), '--workspace', str(workspace), '--json', 'run',
        'Return OWNED_RETRY_OK without using tools.'], env=env, cwd=workspace,
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    output = bytearray()
    overflow = threading.Event()

    def drain():
        while chunk := process.stdout.read1(4096):
            room = 128 * 1024 - len(output)
            output.extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if process.poll() is None:
                    process.kill()

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        code = process.wait(timeout=60)
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
checks, details = {}, {}
try:
    with tempfile.TemporaryDirectory(prefix='rook-http-date-native-') as directory:
        root = Path(directory)
        intact, bounded = True, True
        for name in cases:
            allowance = 20000
            case = root / name
            case.mkdir()
            home = case / 'home'; home.mkdir()
            workspace = case / 'workspace'; workspace.mkdir()
            empty = case / 'empty'; empty.mkdir()
            sentinel = workspace / 'sentinel'
            sentinel.write_bytes(b'owned unchanged')
            (home / 'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
max_output_tokens={allowance}
[sandbox]
stance='autonomous'
[models.fixture]
api='openai'
model='owned-credit'
context_window=65536
url='http://127.0.0.1:{server.server_port}/{name}/v1'
''', encoding='utf-8')
            env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
                ROOK_BUILTIN_SKILLS=str(empty), ROOK_LOG='error', PATH=str(empty))
            code, text, limited = invoke(workspace, env)
            record = seen[name]
            expected = [20000, 20000]
            timing = (len(record['main_wall']) == 2
                      and (name != 'date' or record['main_wall'][1] >= record['not_before_wall'])
                      and (name != 'seconds' or record['main_mono'][1] >= record['not_before_mono']))
            checks[name] = record['main_caps'] == expected and code == 0 and 'OWNED_RETRY_OK' in text and timing
            intact &= sentinel.read_bytes() == b'owned unchanged' and set(workspace.iterdir()) == {sentinel}
            bounded &= limited
            details[name] = {'exit': code, 'observed': record, 'output_tail': text[-4000:]}
        checks['owned_workspace_unchanged'] = intact
        checks['bounded_capture_and_local_requests'] = bounded and not errors
    checks['binary_unchanged'] = fingerprint() == digest
    report = {'binary_sha256': digest, 'checks': checks, 'cases': details, 'errors': errors,
              'scope': 'Owned Retry-After HTTP refusals and actual CLI only; no real model, tool effect or installed daemon.'}
    with args.output.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps({'binary_sha256': digest, 'checks': checks}), flush=True)
    assert all(checks.values()), checks
finally:
    server.shutdown()
    server.server_close()
