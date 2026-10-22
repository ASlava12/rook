"""Measure ordinary retry clustering with four owned CLI processes and a local API."""
import concurrent.futures
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
BINARY = args.bin_dir.resolve() / ('rook.exe' if os.name == 'nt' else 'rook')
REPORT = args.output.resolve()
if not __debug__ or not BINARY.is_file() or REPORT.exists() or not REPORT.parent.is_dir():
    parser.error('Unoptimized Python, an actual CLI and a fresh output path are required.')


def fingerprint():
    with BINARY.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


digest = fingerprint()
barrier = threading.Barrier(4, timeout=30)
lock = threading.Lock()
seen = {str(index): {'main': 0, 'auxiliary': 0} for index in range(4)}
errors = []


class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, *unused):
        pass

    def reply(self, value, status=200, stream=False):
        body = (('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n') if stream
                else json.dumps(value)).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.reply({'data': [{'id': 'owned-concurrent', 'context_length': 65536}]})

    def do_POST(self):
        try:
            arrived = time.monotonic()
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            case = self.path.split('/')[1]
            stream = bool(request.get('stream'))
            with lock:
                record = seen[case]
                assert record['main'] + record['auxiliary'] < 6
                category = 'main' if stream else 'auxiliary'
                record[category] += 1
                index = record[category]
                if stream and index == 2:
                    record['retry_arrival'] = arrived
            if stream and index == 1:
                barrier.wait()
                with lock:
                    record['refusal_sent'] = time.monotonic()
                    record['barrier_released'] = True
                self.reply({'error': {'message': 'owned temporary rate limit', 'code': 429}}, 429)
                return
            text = 'OWNED_CONCURRENT_OK' if stream else '{"action":"finish"}'
            self.reply({'model': 'owned-concurrent', 'choices': [{'index': 0,
                'delta' if stream else 'message': {'role': 'assistant', 'content': text},
                'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1}},
                stream=stream)
        except Exception as error:
            with lock:
                if len(errors) < 8:
                    errors.append(repr(error)[:1000])
            self.close_connection = True


def invoke(case, directory, port):
    root = directory / case
    root.mkdir()
    for name in ['home', 'workspace', 'empty']:
        (root / name).mkdir()
    workspace = root / 'workspace'
    sentinel = workspace / 'sentinel'
    sentinel.write_bytes(b'owned unchanged')
    (root / 'home/config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
[sandbox]
stance='autonomous'
[models.fixture]
api='openai'
model='owned-concurrent'
context_window=65536
url='http://127.0.0.1:{port}/{case}/v1'
''', encoding='utf-8')
    env = dict(os.environ, ROOK_HOME=str(root / 'home'), ROOK_CONFIG_DIR=str(root / 'home'),
        ROOK_BUILTIN_SKILLS=str(root / 'empty'), ROOK_LOG='error', PATH=str(root / 'empty'))
    process = subprocess.Popen([str(BINARY), '--workspace', str(workspace), '--json', 'run',
        'Return OWNED_CONCURRENT_OK without using tools.'], env=env, cwd=workspace,
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
    assert not reader.is_alive() and not overflow.is_set()
    assert code == 0 and b'OWNED_CONCURRENT_OK' in output
    assert sentinel.read_bytes() == b'owned unchanged' and set(workspace.iterdir()) == {sentinel}
    return {'exit': code, 'captured_bytes': len(output), 'workspace_unchanged': True}


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='rook-retry-jitter-') as directory:
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            jobs = {case: pool.submit(invoke, case, Path(directory), server.server_port) for case in seen}
            results = {case: job.result(timeout=75) for case, job in jobs.items()}
    assert not errors and fingerprint() == digest
    assert all(record['main'] == 2 and record['auxiliary'] == 1 for record in seen.values())
    for record in seen.values():
        record['retry_gap_seconds'] = record['retry_arrival'] - record['refusal_sent']
    arrivals = [record['retry_arrival'] for record in seen.values()]
    spread = max(arrivals) - min(arrivals)
    checks = {
        'four_owned_clients_finished': len(results) == 4 and all(r['exit'] == 0 for r in results.values()),
        'one_physical_retry_per_client': all(r['main'] == 2 for r in seen.values()),
        'one_classifier_per_client': all(r['auxiliary'] == 1 for r in seen.values()),
        'provider_minimum_preserved': all(r['retry_gap_seconds'] >= 1 for r in seen.values()),
        'local_request_and_capture_bounds': not errors and all(r['captured_bytes'] <= 128 * 1024 for r in results.values())
            and all(r['main'] + r['auxiliary'] < 6 for r in seen.values()),
        'concurrent_refusals_synchronized': all(r.get('barrier_released') is True for r in seen.values()),
        'owned_workspaces_unchanged': all(r['workspace_unchanged'] is True for r in results.values()),
        'binary_unchanged': fingerprint() == digest,
    }
    report = {'checks': checks, 'binary': str(BINARY), 'binary_sha256': digest, 'cases': seen, 'processes': results,
        'retry_arrival_spread_seconds': spread, 'errors': errors,
        'scope': 'Four owned simultaneous HTTP 429 refusals without Retry-After. Spread is observational, not a statistical gate or production throughput/model-quality evidence.'}
    with REPORT.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps(report), flush=True)
    assert len(checks) == 8 and all(checks.values()), report
finally:
    server.shutdown()
    server.server_close()
