#!/usr/bin/env python3
"""One native local-LLM browser task, with scratch state and retained failures."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--endpoint', default='http://192.168.1.46:1234/v1')
    parser.add_argument('--model', default='huihui-ornith-1.5-35b-a3b-abliterated-i1')
    parser.add_argument('--mcp-cli', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    home, workspace = output / 'home', output / 'workspace'
    home.mkdir(); workspace.mkdir()
    payload = f'rook-browser-{uuid4().hex[:12]}'
    confirmation = f'saved-{uuid4().hex[:12]}'
    submissions = []
    lock = threading.Lock()

    class App(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, code, body):
            body = body.encode()
            self.send_response(code)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.reply(200, '<!doctype html><title>Rook browser probe</title>'
                       '<label>Message<input id="message"></label><button id="save">Save</button>'
                       '<p id="result">Waiting</p><script>document.querySelector("#save").onclick='
                       'async()=>{const r=await fetch("/save",{method:"POST",body:document.querySelector("#message").value});'
                       'document.querySelector("#result").textContent=await r.text();};</script>')

        def do_POST(self):
            size = int(self.headers.get('Content-Length', '0'))
            if self.path != '/save' or not 0 <= size <= 16384:
                self.reply(400, 'invalid submission'); return
            value = self.rfile.read(size).decode('utf-8')
            with lock:
                if len(submissions) >= 32:
                    self.reply(429, 'submission limit'); return
                submissions.append(value)
            self.reply(200, f'Saved: {confirmation}')

    server = ThreadingHTTPServer(('127.0.0.1', 0), App)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{server.server_port}/'
    launch = 'command = "npx"\nargs = ["--yes", "@playwright/mcp@0.0.83"]'
    if args.mcp_cli:
        launch = f'command = "node"\nargs = [{json.dumps(str(args.mcp_cli.resolve()))}]'
    config = f'''[agent]
model = "lan"
max_steps = 12
max_output_tokens = 1536
max_turn_tokens = 64000
plan_first = false
lazy_tools = false
install_servers = false
context_window = 32768
effort = "low"
[memory]
enabled = false
[web]
enabled = false
[browser]
enabled = true
startup_timeout_secs = 120
{launch}
[models.lan]
api = "openai"
url = {json.dumps(args.endpoint)}
model = {json.dumps(args.model)}
key = ""
proxy = "direct"
'''
    (home / 'config.toml').write_text(config)
    env = dict(os.environ, ROOK_HOME=str(home))
    env.pop('ROOK_CONFIG_DIR', None)
    prompt = (f'Используй только инструмент browser. Открой {url}, прочитай snapshot, '
              f'введи в Message ровно {payload}, нажми Save ровно один раз, прочитай '
              'появившееся подтверждение, сделай скриншот и закрой браузер. '
              'В ответе сообщи код подтверждения со страницы. Не изменяй файлы и не делегируй задачу.')
    (output / 'prompt.txt').write_text(prompt)
    began = time.monotonic()
    with (output / 'turn.stdout').open('wb') as stdout, (output / 'turn.stderr').open('wb') as stderr:
        process = subprocess.Popen([str(binary), '--workspace', str(workspace), '--yes', '--json', 'run', prompt],
                                   env=env, stdout=stdout, stderr=stderr, start_new_session=True)
        timed_out = False
        try:
            process.wait(timeout=300)
        except subprocess.TimeoutExpired:
            timed_out = True
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
    server.shutdown(); server.server_close()
    path = output / 'turn.stdout'
    parse_error = None
    try:
        if path.stat().st_size > 1024 * 1024:
            raise ValueError('native result exceeds the inspection bound')
        result = json.loads(path.read_text()) if not timed_out else {}
    except (ValueError, UnicodeError) as error:
        parse_error, result = str(error), {}
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'model': args.model,
              'endpoint': args.endpoint, 'elapsed_secs': time.monotonic() - began,
              'exit_code': process.returncode, 'timed_out': timed_out,
              'payload': payload, 'confirmation': confirmation, 'submissions': submissions,
              'native_result': result, 'parse_error': parse_error,
              'workspace_files': sorted(p.name for p in workspace.iterdir())}
    report['passed'] = (process.returncode == 0 and not timed_out and submissions == [payload]
                        and confirmation in str(result) and not report['workspace_files'])
    (output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps({k: report[k] for k in ('passed', 'elapsed_secs', 'exit_code', 'timed_out', 'submissions')}, ensure_ascii=False))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
