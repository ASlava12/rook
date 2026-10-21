"""Owned native CLI/Chromium dialog probe; scripted HTTP provider, no real model."""
import argparse
from collections import deque
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import subprocess
import threading
import time
from urllib.parse import parse_qs, urlparse

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--mcp-cli', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__:
    parser.error('Run the bounded dialog probe without Python optimization (-O).')
binary = args.binary.resolve()
assert binary.is_file() and args.mcp_cli.is_file()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
home, workspace = output / 'home', output / 'workspace'
home.mkdir(); workspace.mkdir()
observed, failures, physical, page_events = [], [], [], []
actions = deque()
cases = [('Alert', True, None, 'alert:closed'),
         ('Confirm', False, None, 'confirm:false'),
         ('Confirm', True, None, 'confirm:true'),
         ('Confirm', True, None, 'confirm:true'),
         ('Prompt', True, 'Rook answer', 'prompt:"Rook answer"'),
         ('Prompt', False, None, 'prompt:null'),
         ('Prompt', True, '', 'prompt:""')]

def body(content):
    if isinstance(content, list):
        content = '\n'.join(part.get('text', '') for part in content if part.get('type') == 'text')
    try:
        value = json.loads(content)
        return value.get('rook_source', {}).get('content', content) if isinstance(value, dict) else content
    except (ValueError, TypeError):
        return content

class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, data, content_type='application/json', status=200):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.startswith('/event?'):
            values = parse_qs(urlparse(self.path).query).get('value', [])
            assert len(values) == 1 and len(page_events) < 8
            page_events.append(values[0])
            self.send(b'', status=204)
            return
        if self.path.startswith('/v1/'):
            self.send(json.dumps({'data': [{'id': 'fixture'}]}).encode())
            return
        html = b'''<!doctype html><title>Owned Rook artifact dialogs</title>
<button id="alert">Alert</button><button id="confirm">Confirm</button><button id="prompt">Prompt</button>
<p id="result">Waiting</p><script>
function saved(text){document.querySelector('#result').textContent=text;fetch('/event?value='+encodeURIComponent(text));}
document.querySelector('#alert').onclick=()=>{alert('Owned alert');saved('alert:closed');};
document.querySelector('#confirm').onclick=()=>{const value=confirm('Owned confirm');saved('confirm:'+value);};
document.querySelector('#prompt').onclick=()=>{const value=prompt('Owned prompt','default');saved('prompt:'+JSON.stringify(value));};
</script>'''
        self.send(html, 'text/html; charset=utf-8')

    def do_POST(self):
        try:
            size = int(self.headers.get('Content-Length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            physical.append(self.path)
            assert len(physical) <= 52, 'unexpected extra model requests'
            messages = request['messages']
            tools = [body(message.get('content', '')) for message in messages if message['role'] == 'tool']
            if tools:
                observed.append(tools[-1])
            checking = any(str(message.get('content', '')).startswith('Classify whether an assistant') for message in messages)
            if checking:
                message = {'role': 'assistant', 'content': '{"action":"finish"}'}
                finish = 'stop'
            elif actions:
                action = dict(actions.popleft())
                label = action.pop('ref_label', None)
                if label:
                    line = next(line for result in reversed(tools) for line in result.splitlines()
                                if label in line and '[ref=' in line)
                    action['ref'] = re.search(r'\[ref=([^\]]+)\]', line)[1]
                if action['action'] == 'dialog':
                    for result in reversed(tools):
                        try:
                            status = json.loads(result)
                        except ValueError:
                            continue
                        if isinstance(status, dict) and isinstance(status.get('context'), str):
                            action['context'] = status['context']
                            break
                    assert 'context' in action, 'missing fresh browser status'
                call = {'id': f'owned-{len(physical)}', 'type': 'function',
                        'function': {'name': 'browser', 'arguments': json.dumps(action)}}
                message = {'role': 'assistant', 'content': '', 'tool_calls': [call]}
                finish = 'tool_calls'
            else:
                verifying = any('VERDICT: holds' in str(item.get('content', '')) and item['role'] == 'user' for item in messages)
                message = {'role': 'assistant', 'content': 'VERDICT: unproven' if verifying else 'Owned dialog observations are recorded.'}
                finish = 'stop'
            usage = {'prompt_tokens': 100, 'completion_tokens': 5, 'total_tokens': 105}
            if request.get('stream'):
                delta = dict(message)
                if 'tool_calls' in delta:
                    delta['tool_calls'] = [dict(index=i, **call) for i, call in enumerate(delta['tool_calls'])]
                first = {'id': 'owned-dialog', 'model': 'fixture', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]}
                last = {'id': 'owned-dialog', 'model': 'fixture', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}], 'usage': usage}
                data = ('data: ' + json.dumps(first) + '\n\ndata: ' + json.dumps(last) + '\n\ndata: [DONE]\n\n').encode()
                self.send(data, 'text/event-stream')
            else:
                self.send(json.dumps({'id': 'owned-dialog', 'model': 'fixture', 'choices': [{'index': 0, 'message': message, 'finish_reason': finish}], 'usage': usage}).encode())
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            failures.append(repr(error))
            self.send(b'{"error":{"message":"owned dialog fixture rejected request"}}', status=400)

server = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f'http://127.0.0.1:{server.server_port}'
actions.append({'action': 'navigate', 'url': url + '/'})
for label, accept, text, expected in cases:
    actions.extend([{'action': 'snapshot'}, {'action': 'click', 'ref_label': f'button "{label}"'},
                    {'action': 'status'}])
    response = {'action': 'dialog', 'accept': accept}
    if text is not None:
        response['text'] = text
    actions.extend([response, {'action': 'wait', 'text': expected}, {'action': 'snapshot'}])
actions.append({'action': 'close'})
config = f'''[agent]
model = "fixture"
max_steps = 64
max_output_tokens = 1024
max_turn_tokens = 2000000
context_window = 128000
plan_first = false
lazy_tools = false
install_servers = false
[memory]
enabled = false
[web]
enabled = false
[browser]
enabled = true
startup_timeout_secs = 120
command = "node"
args = [{json.dumps(str(args.mcp_cli.resolve()))}]
[models.fixture]
api = "openai"
url = "{url}/v1"
model = "fixture"
key = ""
proxy = "direct"
'''
(home / 'config.toml').write_text(config, encoding='utf-8')
env = dict(os.environ, ROOK_HOME=str(home), ROOK_LOG='error')
env.pop('ROOK_CONFIG_DIR', None)
began = time.monotonic()
process = None
timed_out = False
try:
    with (output / 'turn.stdout').open('wb') as stdout, (output / 'turn.stderr').open('wb') as stderr:
        process = subprocess.Popen([str(binary), '--workspace', str(workspace), '--yes', '--json', 'run',
                                    'Use browser only to inspect and answer the owned local dialogs, then close.'],
                                   env=env, stdout=stdout, stderr=stderr)
        try:
            process.wait(timeout=300)
        except subprocess.TimeoutExpired:
            timed_out = True
            process.terminate()
            process.wait(timeout=20)
finally:
    if process is not None and process.poll() is None:
        process.kill(); process.wait(timeout=20)
    server.shutdown(); server.server_close()
raw = output / 'turn.stdout'
assert raw.stat().st_size <= 1024 * 1024, 'native answer exceeds inspection bound'
try:
    result = json.loads(raw.read_text(encoding='utf-8'))
except (ValueError, UnicodeError):
    result = {}
combined = '\n'.join(observed)
checks = {'native_exit': process.returncode == 0 and not timed_out,
          'ordinary_end_turn': result.get('outcome', result).get('stopped') == 'end_turn',
          'all_scripted_calls_consumed': not actions,
          'seven_dialog_results': all(expected in combined for _, _, _, expected in cases),
          'seven_exact_page_events_in_order': page_events == [expected for _, _, _, expected in cases],
          'no_fixture_errors': not failures,
          'no_unknown_outcomes': 'outcome is unknown' not in combined,
          'no_missing_dialog': 'No dialog visible' not in combined,
          'no_cycle_refusal': 'tool-cycle guard' not in combined,
          'workspace_untouched': not list(workspace.iterdir())}
with binary.open('rb') as stream:
    digest = hashlib.file_digest(stream, 'sha256').hexdigest()
report = {'checks': checks, 'passed': all(checks.values()), 'binary_sha256': digest,
          'elapsed_secs': time.monotonic() - began, 'model_requests': len(physical),
          'dialog_cases': len(cases), 'errors': failures, 'native_result': result}
report['page_events'] = page_events
report['verification_scope'] = 'Scripted provider; independent model verification and task quality are not established. Page events independently confirm the seven dialog resolutions.'
(output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
(output / 'browser-results.json').write_text(json.dumps(observed, ensure_ascii=False), encoding='utf-8')
print(json.dumps({key: report[key] for key in ('checks', 'passed', 'elapsed_secs', 'model_requests', 'dialog_cases', 'errors')}))
raise SystemExit(0 if report['passed'] else 1)
