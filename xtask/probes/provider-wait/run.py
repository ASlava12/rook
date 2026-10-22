"""Observe one real owned daemon during a provider-requested backoff; no user daemon."""
import argparse
import contextlib
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
parser.add_argument('--refuse', choices=('main', 'auxiliary'), default='main')
args = parser.parse_args()
refuse_stream = args.refuse == 'main'
assert __debug__ and not args.output.exists()
root = Path(__file__).resolve().parent
bins = args.bin_dir.resolve()
suffix = '.exe' if os.name == 'nt' else ''
def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()
binaries = {name: {'path': str(bins / (name + suffix)),
    'sha256': sha(bins / (name + suffix))} for name in ('rook', 'rookd')}
calls, errors = [], []
lock = threading.Lock()
class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)
    def log_message(self, *unused): pass
    def reply(self, value, status=200, stream=False, retry=None):
        body = (('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n') if stream
                else json.dumps(value)).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length', str(len(body)))
        if retry is not None: self.send_header('Retry-After', str(retry))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self): self.reply({'data': [{'id': 'owned-wait'}]})
    def do_POST(self):
        try:
            size = int(self.headers.get('Content-Length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            stream = bool(request.get('stream'))
            with lock:
                assert len(calls) < 6
                index = sum(c['stream'] == stream for c in calls) + 1
                call = {'stream': stream, 'index': index, 'epoch': time.time(), 'monotonic': time.monotonic()}
                calls.append(call)
            if stream == refuse_stream and index == 1:
                call['refusal_sent'] = time.time()
                call['refusal_monotonic'] = time.monotonic()
                self.reply({'error': {'message': 'owned provider backoff', 'code': 429}}, 429, retry=45)
                return
            assert index == (2 if stream == refuse_stream else 1)
            text = 'OWNED_PROVIDER_WAIT_OK' if stream else '{"action":"finish"}'
            self.reply({'model': 'owned-wait', 'choices': [{'index': 0,
                'delta' if stream else 'message': {'role': 'assistant', 'content': text},
                'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}, stream=stream)
        except Exception as error:
            if len(errors) < 8:
                errors.append(repr(error)[:1000])
            self.close_connection = True
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
daemon = observer = None
report = {'refused_purpose': args.refuse, 'binaries': binaries, 'calls': calls, 'server_errors': errors, 'status': 'incomplete'}
try:
    with contextlib.ExitStack() as cleanup:
        temporary = cleanup.enter_context(tempfile.TemporaryDirectory(prefix='rook-provider-wait-', dir=args.tmp_root))
        def stop_owned():
            for process in (observer, daemon):
                if process is not None and process.poll() is None:
                    process.kill(); process.wait(timeout=10)
        cleanup.callback(stop_owned)
        directory = Path(temporary)
        home, workspace = directory / 'home', directory / 'workspace'
        home.mkdir(); workspace.mkdir()
        sentinel = workspace / 'sentinel'
        sentinel.write_bytes(b'owned unchanged')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_LOG='error')
        (home / 'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
[telemetry]
stall_after_secs=30
[server]
port=0
[models.fixture]
api='openai'
model='owned-wait'
context_window=32768
url='http://127.0.0.1:{server.server_port}/v1'
''', encoding='utf-8')
        cli = [binaries['rook']['path'], '--workspace', str(workspace)]
        def run(*arguments, **kw):
            return subprocess.check_output(cli + list(arguments), cwd=workspace, env=env,
                timeout=30, **kw)
        before = {row['id'] for row in json.loads(run('--json', 'session', 'ls'))}
        run('chat', input=b'/quit\n', stderr=subprocess.DEVNULL)
        after = {row['id'] for row in json.loads(run('--json', 'session', 'ls'))}
        assert len(after - before) == 1
        session = (after - before).pop()
        daemon = subprocess.Popen([binaries['rookd']['path'], '--workspace', str(workspace), '--port', '0'],
            cwd=workspace, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 30
        while True:
            assert daemon.poll() is None, 'owned daemon stopped'
            assert time.monotonic() < deadline, 'owned daemon startup deadline'
            try:
                base = (home / 'rookd.addr').read_text().strip()
                with opener.open(base + '/api/health', timeout=2) as response: json.load(response)
                break
            except (OSError, ValueError): time.sleep(.05)
        captured = directory / 'observation.json'
        observer = subprocess.Popen(['node', str(root / 'chat.mjs'), base, session, str(captured)],
            cwd=workspace, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        report['diagnostics'] = []
        observed_until = time.monotonic() + 120
        while observer.poll() is None:
            assert time.monotonic() < observed_until, 'owned observation deadline'
            with opener.open(base + '/api/sessions/' + session + '/diagnostics?logs=false', timeout=3) as response:
                raw = response.read(1024 * 1024 + 1)
                assert len(raw) <= 1024 * 1024
                activity = json.loads(raw).get('activity')
            if activity is not None:
                report['diagnostics'].append({'epoch': time.time(), 'state': activity})
            assert len(report['diagnostics']) <= 120
            time.sleep(1)
        code = observer.returncode
        observation = json.loads(captured.read_bytes())
        report['observation'] = observation
        assert code == 0 and observation['error'] is None, observation
        assert not errors and sum(c['stream'] == refuse_stream for c in calls) == 2
        main = [c for c in calls if c['stream'] == refuse_stream]
        report['retry_gap_seconds'] = main[1]['monotonic'] - main[0]['refusal_monotonic']
        assert report['retry_gap_seconds'] >= 45
        report['stalls_during_expected_backoff'] = [e for e in observation['events']
            if e['type'] == 'activity' and e.get('state', {}).get('stalled')
            and main[0]['refusal_sent'] <= e['epoch'] < main[1]['epoch']]
        report['workspace_unchanged'] = sentinel.read_bytes() == b'owned unchanged' and set(workspace.iterdir()) == {sentinel}
        assert report['workspace_unchanged']
        report['status'] = 'completed'
        # Release the owned store before the temporary directory is removed.
        daemon.terminate(); daemon.wait(timeout=20); daemon = None
finally:
    for process in (observer, daemon):
        if process is not None and process.poll() is None:
            process.kill(); process.wait(timeout=10)
    server.shutdown(); server.server_close()
    report['binaries_unchanged'] = all(sha(Path(v['path'])) == v['sha256'] for v in binaries.values())
    if report['status'] == 'completed':
        waits = [row['state'] for row in report['diagnostics'] if row['state'].get('provider_wait')]
        physical = [call for call in calls if call['stream'] == refuse_stream]
        visible = [event for event in report['observation']['events'] if 'provider retry' in event.get('said', '')]
        report['checks'] = {
            'physical_retry_minimum_preserved': report['retry_gap_seconds'] >= 45,
            'exact_physical_call_counts': len(calls) == 3 and len(physical) == 2,
            'provider_status_visible_in_chat': bool(visible) and any('HTTP 429' in e['said'] and 'attempt 2' in e['said'] for e in visible),
            'deadline_is_current_and_scoped': bool(waits) and len({(row['session'], row['generation'], row['provider_wait']['until_ms']) for row in waits}) == 1 and all(
                row['provider_wait']['status'] == 429 and row['provider_wait']['next_attempt'] == 2
                and physical[0]['refusal_sent'] + 44 <= row['provider_wait']['until_ms'] / 1000 <= physical[1]['epoch'] + 1
                for row in waits),
            'status_does_not_invent_progress': bool(waits) and max(row['idle_secs'] for row in waits) >= 40 and len({row['last_progress_at'] for row in waits}) == 1,
            'expected_wait_does_not_stall': bool(waits) and not report['stalls_during_expected_backoff'] and all(not row['stalled'] and row['notifications'] == 0 and row.get('retry_at') is None for row in waits),
            'workspace_unchanged': report['workspace_unchanged'],
            'binaries_unchanged': report['binaries_unchanged'],
        }
    args.output.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
assert report['status'] == 'completed' and all(report['checks'].values()), report.get('checks')
print(json.dumps({'status': report['status'], 'stalls_during_backoff': len(report['stalls_during_expected_backoff']),
    'retry_gap_seconds': report['retry_gap_seconds'], 'report': str(args.output)}))
