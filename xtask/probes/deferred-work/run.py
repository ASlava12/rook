#!/usr/bin/env python3
"""Real owned daemon/process-loss probe; no user store, secret or real model."""
import argparse
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.error

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
args = parser.parse_args()
binary = args.bin_dir.resolve()
suffix = '.exe' if os.name == 'nt' else ''
for name in ['rook', 'rookd']:
    assert (binary / (name + suffix)).is_file()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
held = threading.Event()
release = threading.Event()
counts = {}
errors = []


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *unused):
        pass

    def do_GET(self):
        self.reply({'data': [{'id': 'A'}]})

    def reply(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2 * 1024 * 1024
            request = json.loads(self.rfile.read(size))
            text = '\n'.join(str(message.get('content', '')) for message in request['messages'])
            # Workspace listings contain earlier fixtures' file names. Route
            # only by the admitted managed goal, not incidental source text.
            marker = next(name for name in ['NATIVE_DEFER_A', 'NATIVE_CAPACITY_B',
                                           'NATIVE_RESTART_C', 'NATIVE_PAUSED_D', 'NATIVE_CONVERSATION_E', 'NATIVE_HUMAN_F']
                          if any(message['role'] == 'user' and name + '\n\nThis is iteration' in str(message.get('content', ''))
                                 for message in request['messages']))
            assert 'Classify whether' not in text, 'a deferred boundary cannot request completion classification'
            # A real provider requires every earlier call to have one result,
            # including calls skipped at the checkpoint boundary.
            pending = set()
            for message in request['messages']:
                if message['role'] == 'tool':
                    assert message['tool_call_id'] in pending, message
                    pending.remove(message['tool_call_id'])
                else:
                    assert not pending, ('unanswered calls in the resumed request', pending)
                    pending.update(call['id'] for call in message.get('tool_calls', []))
            assert not pending
            assert any(tool['function']['name'] == 'defer_work' for tool in request['tools'])
            counts[marker] = counts.get(marker, 0) + 1
            assert sum(counts.values()) <= 24
            resumed = any(message['role'] == 'tool' and 'Work checkpoint saved' in str(message.get('content', ''))
                          for message in request['messages'])
            calls = []
            if marker == 'NATIVE_HUMAN_F':
                initial = counts[marker] == 1
                if initial:
                    calls.append(self.call('before', 'write_file', {'path': marker + '.txt', 'content': 'one write'}))
                calls.append(self.call('human', 'block_work', {'reason': 'Manual login required.' if initial else 'Manual verification still required.'}))
                calls.append(self.call('tail', 'write_file', {'path': marker + '-forbidden.txt', 'content': 'tail'}))
            elif marker == 'NATIVE_CAPACITY_B':
                held.set()
                assert release.wait(90), 'owned capacity gate expired'
                delay, reason = 60, 'capacity released'
            elif resumed:
                delay, reason = 60, 'resumed same transcript'
            else:
                delay = {'NATIVE_DEFER_A': 20, 'NATIVE_RESTART_C': 8, 'NATIVE_PAUSED_D': 2, 'NATIVE_CONVERSATION_E': 3}[marker]
                reason = 'initial wait'
                calls.append(self.call('before', 'write_file', {'path': marker + '.txt', 'content': 'one write'}))
            if marker != 'NATIVE_HUMAN_F':
                calls.append(self.call('wait', 'defer_work', {'seconds': delay, 'reason': reason}))
                if not resumed and marker != 'NATIVE_CAPACITY_B':
                    calls.append(self.call('tail', 'write_file', {'path': marker + '-forbidden.txt', 'content': 'tail'}))
            if request.get('stream'):
                delta = {'role': 'assistant', 'tool_calls': [dict(index=index, **call) for index, call in enumerate(calls)]}
                first = {'id': 'native-defer', 'model': 'A', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]}
                last = {'id': 'native-defer', 'model': 'A', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'tool_calls'}],
                        'usage': {'prompt_tokens': 100, 'completion_tokens': 5, 'total_tokens': 105}}
                data = ('data: ' + json.dumps(first) + '\n\ndata: ' + json.dumps(last) + '\n\ndata: [DONE]\n\n').encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            else:
                self.reply({'id': 'native-defer', 'model': 'A', 'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': '', 'tool_calls': calls}, 'finish_reason': 'tool_calls'}], 'usage': {'prompt_tokens': 100, 'completion_tokens': 5, 'total_tokens': 105}})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            errors.append(repr(error))
            self.send_error(500, 'owned fixture rejected the request')

    @staticmethod
    def call(identifier, name, arguments):
        return {'id': identifier, 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(arguments)}}


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
threading.Thread(target=server.serve_forever, daemon=True).start()
daemon = None
observers = []
temporary = tempfile.TemporaryDirectory(prefix='rook-deferred-', dir=args.tmp_root)
try:
    directory = temporary.name
    root = Path(directory)
    home = root / 'home'; home.mkdir()
    workspace = root / 'workspace'; workspace.mkdir()
    other_workspace = root / 'other-workspace'; other_workspace.mkdir()
    env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), TMPDIR=str(root), ROOK_LOG='error')
    (home / 'config.toml').write_text(f'''[agent]
model='A'
install_servers=false
plan_first=false
one_script=false
max_steps=4
[work]
max_parallel_runs=1
[server]
port=0
[models.A]
model='A'
api='openai'
url='http://127.0.0.1:{server.server_port}/v1'
context_window=32768
''')
    base = ''

    def api(path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(base + path, data=data,
            headers={'Content-Type': 'application/json'} if data else {})
        try:
            with opener.open(request, timeout=10) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            detail = error.read(8192).decode('utf-8', errors='replace')
            raise AssertionError((path, error.code, detail)) from error

    def until(check, label, seconds=40):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            assert not errors, errors
            value = check()
            if value:
                return value
            time.sleep(.05)
        raise AssertionError(label)

    def start():
        global daemon, base
        daemon = subprocess.Popen([str(binary / ('rookd' + suffix)), '--workspace', str(workspace), '--port', '0'],
                                  env=env, cwd=workspace, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            assert daemon.poll() is None, 'owned daemon stopped during startup'
            try:
                base = (home / 'rookd.addr').read_text().strip()
                api('/api/health')
                return
            except (OSError, ValueError):
                time.sleep(.05)
        raise AssertionError('owned daemon startup expired')

    def submit(marker, project=workspace):
        return api('/api/work', {'goal': marker, 'workspace': str(project), 'autonomous': True,
                                'max_iterations': 10, 'max_tokens': 10000, 'max_seconds': 300})

    def view(run):
        return api('/api/work/' + run['id'])

    def wait(run, reason):
        def ready():
            saved = view(run)
            assert saved['status'] not in ['blocked', 'limited', 'retry_wait'], saved
            return saved if saved.get('waiting', {}).get('reason') == reason else None
        return until(ready, 'managed checkpoint was not saved')

    def cancel(run):
        result = api('/api/work/' + run['id'] + '/control', 'cancel')
        assert result['status'] == 'cancelled' and not result.get('waiting'), result

    def observe(session, name, mode='observe'):
        output = root / (name + '.json')
        child = subprocess.Popen(['node', str(Path(__file__).with_name('chat.mjs').resolve()),
                                  base, session, str(output), mode], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        observers.append(child)
        return child, output

    def observation(output):
        return json.loads(output.read_text()) if output.exists() else {}

    def complete_observer(child, output):
        _, diagnostic = child.communicate(timeout=30)
        assert child.returncode == 0, (observation(output), diagnostic)

    start()
    first = submit('NATIVE_DEFER_A')
    saved = wait(first, 'initial wait')
    session = saved['session']
    deadline = saved['waiting']['until']
    until(lambda: api('/api/health')['turns_running'] == 0, 'wait retained a live executor')
    # Existing single-writer workspace ownership remains in force. The
    # released daemon capacity admits unrelated work in another project.
    other = submit('NATIVE_CAPACITY_B', other_workspace)
    assert held.wait(30), 'waiting run did not release the single managed slot'
    while time.time() <= deadline + 1:
        assert not errors, errors
        time.sleep(.05)
    assert counts['NATIVE_DEFER_A'] == 1, 'due work bypassed the occupied capacity slot'
    assert view(first)['status'] == 'queued', view(first)
    release.set()
    resumed = wait(first, 'resumed same transcript')
    assert resumed['session'] == session and resumed['iterations'] == 2 and resumed['tokens'] == 210, resumed
    assert counts['NATIVE_DEFER_A'] == 2
    assert (workspace / 'NATIVE_DEFER_A.txt').read_text() == 'one write'
    assert not (workspace / 'NATIVE_DEFER_A-forbidden.txt').exists()
    until(lambda: api('/api/health')['turns_running'] == 0, 'wait owners did not retire')
    cancel(first); cancel(other)

    recovery = submit('NATIVE_RESTART_C')
    before = wait(recovery, 'initial wait')
    until(lambda: api('/api/health')['turns_running'] == 0, 'checkpoint owner did not retire')
    daemon.kill(); daemon.wait(timeout=10); daemon = None
    start()
    after = wait(recovery, 'resumed same transcript')
    assert after['session'] == before['session'] and after['iterations'] == 2 and after['tokens'] == 210, after
    assert after['restart_guard']['consecutive_interruptions'] == 0, after
    assert counts['NATIVE_RESTART_C'] == 2
    assert (workspace / 'NATIVE_RESTART_C.txt').read_text() == 'one write'
    assert not (workspace / 'NATIVE_RESTART_C-forbidden.txt').exists()
    shown = subprocess.check_output([str(binary / ('rook' + suffix)), '--workspace', str(workspace), 'task', 'show', recovery['id']], env=env, cwd=workspace, text=True, timeout=30)
    assert 'saved wait:' in shown and 'resumed same transcript' in shown, shown
    until(lambda: api('/api/health')['turns_running'] == 0, 'resumed owner did not retire')
    cancel(recovery)

    paused = submit('NATIVE_PAUSED_D')
    before = wait(paused, 'initial wait')
    api('/api/work/' + paused['id'] + '/control', 'pause')
    while time.time() <= before['waiting']['until'] + 1:
        time.sleep(.05)
    assert counts['NATIVE_PAUSED_D'] == 1 and view(paused)['status'] == 'paused'
    cancel(paused)

    human = submit('NATIVE_HUMAN_F')
    before = until(lambda: view(human) if view(human)['status'] == 'blocked' else None, 'human blocker was not saved')
    assert before['reason'] == before['reply'] == 'Manual login required.' and before['iterations'] == 1, before
    assert before.get('next_attempt_at') is None and not before.get('waiting'), before
    until(lambda: api('/api/health')['turns_running'] == 0, 'human blocker retained its executor')
    daemon.kill(); daemon.wait(timeout=10); daemon = None
    start()
    after = view(human)
    assert after['status'] == 'blocked' and after['session'] == before['session'], after
    assert counts['NATIVE_HUMAN_F'] == 1, 'daemon restart must not run a human blocker'
    api('/api/work/' + human['id'] + '/control', 'resume')
    after = until(lambda: view(human) if view(human)['iterations'] == 2 else None, 'explicit resume did not continue the human checkpoint')
    assert after['status'] == 'blocked' and after['reason'] == 'Manual verification still required.', after
    assert after['session'] == before['session'] and counts['NATIVE_HUMAN_F'] == 2, after
    assert (workspace / 'NATIVE_HUMAN_F.txt').read_text() == 'one write'
    assert not (workspace / 'NATIVE_HUMAN_F-forbidden.txt').exists()
    until(lambda: api('/api/health')['turns_running'] == 0, 'resumed human blocker retained its executor')
    cancel(human)

    # Create an ordinary empty chat while this owned store is unlocked,
    # then promote it through the same persisted conversation-goal API.
    until(lambda: api('/api/health')['turns_running'] == 0, 'paused owner did not retire')
    daemon.terminate(); daemon.wait(timeout=20); daemon = None
    cli = [str(binary / ('rook' + suffix)), '--workspace', str(workspace)]
    before_ids = {row['id'] for row in json.loads(subprocess.check_output(cli + ['--json', 'session', 'ls'], env=env, cwd=workspace, text=True, timeout=30))}
    empty = subprocess.run(cli + ['chat'], input='/quit\n', capture_output=True, text=True, env=env, cwd=workspace, timeout=30)
    assert empty.returncode == 0, (empty.stdout, empty.stderr)
    after_ids = {row['id'] for row in json.loads(subprocess.check_output(cli + ['--json', 'session', 'ls'], env=env, cwd=workspace, text=True, timeout=30))}
    new_ids = after_ids - before_ids
    assert len(new_ids) == 1, new_ids
    chat_session = new_ids.pop()
    start()
    conversation = api('/api/work', {'goal': 'NATIVE_CONVERSATION_E', 'workspace': str(workspace),
        'autonomous': True, 'max_iterations': 10, 'max_tokens': 10000, 'max_seconds': 300,
        'conversation': {'session': chat_session, 'model': 'A', 'effort': 'medium', 'stance': 'autonomous', 'options': {}}})
    initial = wait(conversation, 'initial wait')
    observer, observation_path = observe(chat_session, 'automatic-chat')
    until(lambda: api('/api/health')['turns_running'] == 0, 'conversation wait retained its chat owner')
    continued = wait(conversation, 'resumed same transcript')
    assert continued['session'] == initial['session'] == chat_session and continued['iterations'] == 2, continued
    until(lambda: api('/api/health')['turns_running'] == 0, 'resumed conversation did not yield its owner')
    assert counts['NATIVE_CONVERSATION_E'] == 2
    assert not (workspace / 'NATIVE_CONVERSATION_E-forbidden.txt').exists()
    complete_observer(observer, observation_path)
    release.clear(); held.clear()
    occupied = submit('NATIVE_CAPACITY_B', other_workspace)
    assert held.wait(30), 'capacity fixture did not acquire the released chat slot'
    observer, observation_path = observe(chat_session, 'early-resume-chat', 'continue')
    until(lambda: observation(observation_path).get('queued') == 1, 'chat resume did not queue behind the other owner')
    assert counts['NATIVE_CONVERSATION_E'] == 2 and view(conversation)['status'] == 'queued'
    assert not view(conversation).get('waiting'), 'explicit resume must clear the old wait'
    release.set()
    complete_observer(observer, observation_path)
    assert counts['NATIVE_CONVERSATION_E'] == 3
    until(lambda: api('/api/health')['turns_running'] == 0, 'resumed chat owners did not retire')
    cancel(occupied)
    cancel(conversation)
    assert not errors, errors
    print(json.dumps({'single_slot_released': True, 'due_wait_respects_other_owner': True,
                      'same_transcript_after_wake': True, 'physical_daemon_restart': True,
                      'previous_file_effect_not_repeated': True, 'unexecuted_batch_tail_absent': True,
                      'wire_history_calls_are_answered': True, 'pause_prevents_due_wake': True,
                      'cancel_clears_wait': True, 'cli_saved_wait_visible': True,
                      'conversation_releases_chat_owner': True, 'conversation_due_wake_same_session': True,
                      'chat_observer_follows_automatic_wake': True, 'early_chat_resume_queues_at_capacity': True,
                      'human_blocker_survives_restart': True, 'human_blocker_requires_explicit_resume': True,
                      'requests': counts}))
    daemon.terminate(); daemon.wait(timeout=20); daemon = None
finally:
    release.set()
    for observer in observers:
        if observer.poll() is None:
            observer.terminate()
            try:
                observer.wait(timeout=5)
            except subprocess.TimeoutExpired:
                observer.kill(); observer.wait()
    if daemon is not None:
        daemon.terminate()
        try:
            daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:
            daemon.kill(); daemon.wait()
    server.shutdown(); server.server_close()
    temporary.cleanup()
