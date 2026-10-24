"""Owned Google/MCP tool-schema checks through the actual CLI and private daemon."""
import argparse
import copy
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import tomllib
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
args = parser.parse_args()
assert __debug__
destination = args.output.resolve()
assert destination.parent.is_dir() and not destination.exists()
root = Path(tempfile.mkdtemp(prefix='rook-google-schema-', dir=args.tmp_root)).resolve()
version = tomllib.loads((Path(__file__).resolve().parents[3] / 'Cargo.toml').read_text('utf-8'))['workspace']['package']['version']
suffix = '.exe' if os.name == 'nt' else ''
bins = {name: args.bin_dir.resolve() / (name + suffix) for name in ['rook', 'rookd']}
sha = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
report = {'status': 'running', 'owned_root': str(root), 'script_sha256': sha(__file__), 'binaries': {}, 'cases': {}, 'checks': {}, 'server_errors': []}
empty = root / 'empty-skills'
empty.mkdir()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
literal = {'const': 'literal-data', 'properties': {'nested': {'const': 3}}}
schema = {'type': 'object', 'properties': {'mode': {'type': 'string', 'const': 'fixed'},
          'count': {'type': 'integer', 'const': 7}, 'payload': {'type': 'object', 'default': literal,
          'examples': [literal], 'x-custom': literal}}, 'required': ['mode', 'count'], 'additionalProperties': False,
          '$defs': {'choice': {'oneOf': [{'type': 'string'}, {'type': 'integer'}]}}}
expected = copy.deepcopy(schema)
for name, value in [('mode', 'fixed'), ('count', 7)]:
    expected['properties'][name].pop('const')
    expected['properties'][name]['enum'] = [value]
retired = 0

try:
    for name, binary in bins.items():
        actual = subprocess.check_output([str(binary), '--version'], timeout=10).decode().strip()
        assert actual == name + ' ' + version
        report['binaries'][name] = {'path': str(binary), 'version': actual, 'sha256': sha(binary)}
    for mode in ['local', 'daemon']:
        case = root / mode
        case.mkdir()
        home, workspace = case / 'home', case / 'workspace'
        home.mkdir()
        workspace.mkdir()
        requests, calls, methods = [], [], []

        class Fixture(http.server.BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def setup(self):
                super().setup()
                self.connection.settimeout(10)

            def answer(self, value, status=200, stream=False):
                data = json.dumps(value).encode()
                if stream:
                    data = b'data: ' + data + b'\n\n'
                self.send_response(status)
                self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                try:
                    length = int(self.headers.get('Content-Length', '0'))
                    assert 0 < length <= 2 * 1024 * 1024
                    value = json.loads(self.rfile.read(length))
                    if self.path == '/mcp':
                        method = value['method']
                        methods.append(method)
                        assert len(methods) <= 32
                        if 'id' not in value:
                            self.send_response(202)
                            self.send_header('Content-Length', '0')
                            self.end_headers()
                            return
                        if method == 'initialize':
                            result = {'protocolVersion': value['params']['protocolVersion'], 'capabilities': {'tools': {}},
                                      'serverInfo': {'name': 'owned-schema', 'version': '1'}}
                        elif method == 'tools/list':
                            result = {'tools': [{'name': 'inspect', 'description': 'Owned schema fixture', 'inputSchema': schema,
                                               'annotations': {'readOnlyHint': True}}]}
                        elif method == 'tools/call':
                            assert value['params']['name'] == 'inspect'
                            calls.append(value['params']['arguments'])
                            result = {'content': [{'type': 'text', 'text': 'OWNED_SCHEMA_TOOL'}], 'isError': False}
                        else:
                            raise AssertionError('unexpected MCP method: ' + method)
                        self.answer({'jsonrpc': '2.0', 'id': value['id'], 'result': result})
                        return
                    assert self.path in ['/v1beta/models/gemini-2.5-pro:generateContent',
                                         '/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse']
                    assert len(requests) < 8
                    declarations = [d for t in value.get('tools', []) for d in t.get('functionDeclarations', [])]
                    auxiliary = not declarations
                    requests.append({'path': self.path, 'credential': self.headers.get('x-goog-api-key') == 'owned-schema-key',
                                     'auxiliary': auxiliary, 'declarations': declarations, 'contents': value.get('contents', [])})
                    main = [r for r in requests if not r['auxiliary']]
                    if auxiliary:
                        parts, finish = [{'text': '{"action":"finish"}'}], 'STOP'
                    elif len(main) == 1:
                        parts, finish = [{'functionCall': {'name': 'owned__inspect', 'args': {'mode': 'fixed', 'count': 7}}}], 'STOP'
                    else:
                        parts, finish = [{'text': 'OWNED_GOOGLE_SCHEMA_OK'}], 'STOP'
                    self.answer({'candidates': [{'content': {'role': 'model', 'parts': parts}, 'finishReason': finish}],
                                 'usageMetadata': {'promptTokenCount': 1, 'candidatesTokenCount': 1},
                                 'modelVersion': 'gemini-2.5-pro'}, stream='streamGenerateContent' in self.path)
                except BaseException as error:
                    if len(report['server_errors']) < 16:
                        report['server_errors'].append(repr(error)[:512])
                    self.close_connection = True

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': .05}, daemon=True)
        thread.start()
        (home / 'config.toml').write_text(f'''skill_sources=[{json.dumps(empty.as_posix())}]
[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
max_turn_secs=45
[memory]
enabled=false
[sandbox]
allow=['owned__inspect']
[models.fixture]
api='google'
model='gemini-2.5-pro'
context_window=131072
key='env:ROOK_SCHEMA_FIXTURE_KEY'
url='http://127.0.0.1:{server.server_port}/v1beta'
[[mcp]]
name='owned'
url='http://127.0.0.1:{server.server_port}/mcp'
refresh_tools=false
''', encoding='utf-8')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_BUILTIN_SKILLS=str(empty),
                   ROOK_SCHEMA_FIXTURE_KEY='owned-schema-key', ROOK_LOG='error', RUST_BACKTRACE='0')

        def invoke(label, arguments):
            out, err = case / (label + '.stdout'), case / (label + '.stderr')
            with out.open('xb') as stdout, err.open('xb') as stderr:
                result = subprocess.run([str(bins['rook']), '-C', str(workspace), *arguments], cwd=workspace, env=env,
                                        stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, timeout=55,
                                        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            assert out.stat().st_size <= 1024 * 1024 and err.stat().st_size <= 1024 * 1024
            return result.returncode, out.read_bytes(), err.read_bytes()

        daemon = daemon_log = None
        try:
            if mode == 'daemon':
                daemon_log = (case / 'daemon.log').open('xb')
                daemon = subprocess.Popen([str(bins['rookd']), '-C', str(workspace), '--bind', '127.0.0.1', '--port', '0'],
                                          env=env, cwd=workspace, stdin=subprocess.DEVNULL, stdout=daemon_log,
                                          stderr=subprocess.STDOUT, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    assert daemon.poll() is None
                    try:
                        address = (home / 'rookd.addr').read_text('utf-8').strip()
                        assert address.startswith('http://127.0.0.1:')
                        with opener.open(address + '/api/health', timeout=1) as response:
                            assert response.status == 200
                        break
                    except (OSError, AssertionError):
                        time.sleep(.05)
                else:
                    raise AssertionError('private daemon did not become ready')
            code, out, err = invoke('run', ['run', 'Call owned__inspect with mode fixed and count 7, then report the result.'])
            main = [r for r in requests if not r['auxiliary']]
            offered = [d for r in main for d in r['declarations'] if d['name'] == 'owned__inspect']
            responses = [p['functionResponse'] for r in main[1:] for c in r['contents'] for p in c.get('parts', []) if 'functionResponse' in p]
            checks = {'client_success': code == 0 and b'OWNED_GOOGLE_SCHEMA_OK' in out,
                      'two_main_requests': len(main) == 2,
                      'original_credential': bool(requests) and all(r['credential'] for r in requests),
                      'mcp_handshake_and_discovery': 'initialize' in methods and 'tools/list' in methods,
                      'exact_mcp_schema_on_wire': len(offered) == 2 and all(d.get('parametersJsonSchema') == expected for d in offered),
                      'legacy_field_absent': bool(main) and all('parameters' not in d for r in main for d in r['declarations']),
                      'scalar_constants_converted': bool(offered) and all(d.get('parametersJsonSchema', {}).get('properties', {}).get('mode') == {'type': 'string', 'enum': ['fixed']} and d.get('parametersJsonSchema', {}).get('properties', {}).get('count') == {'type': 'integer', 'enum': [7]} for d in offered),
                      'literal_values_preserved': bool(offered) and all(d.get('parametersJsonSchema', {}).get('properties', {}).get('payload') == schema['properties']['payload'] for d in offered),
                      'original_tool_arguments_executed_once': calls == [{'mode': 'fixed', 'count': 7}],
                      'tool_result_reaches_continuation': len(responses) == 1 and responses[0]['name'] == 'owned__inspect' and 'OWNED_SCHEMA_TOOL' in json.dumps(responses[0]),
                      'workspace_unchanged': not any(workspace.iterdir())}
            report['cases'][mode] = {'checks': checks, 'client_exit': code, 'requests': requests, 'tool_calls': calls, 'mcp_methods': methods,
                                     'stdout': str(case / 'run.stdout'), 'stderr': str(case / 'run.stderr')}
            report['checks'].update({mode + '_' + k: v for k, v in checks.items()})
            print(json.dumps({'case': mode, 'checks': checks}), flush=True)
            assert all(v is True for v in checks.values()), checks
            if daemon is not None:
                stop, _, _ = invoke('stop', ['daemon', 'stop'])
                assert stop == 0 and daemon.wait(timeout=10) == 0
                retired += 1
                daemon = None
        finally:
            if daemon is not None:
                if daemon.poll() is None:
                    daemon.terminate()
                daemon.wait(timeout=10)
            if daemon_log is not None:
                daemon_log.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
            assert not thread.is_alive()
    report['checks'].update(binary_pair_unchanged=all(sha(bins[name]) == r['sha256'] for name, r in report['binaries'].items()),
                            no_server_errors=not report['server_errors'], private_daemon_retired=retired == 1)
    report['status'] = 'passed' if all(v is True for v in report['checks'].values()) else 'failed'
except BaseException as error:
    report['status'] = 'failed'
    report['error'] = repr(error)
    raise
finally:
    destination.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
print(json.dumps({'status': report['status'], 'conditions': len(report['checks']), 'report': str(destination)}), flush=True)
raise SystemExit(0 if report['status'] == 'passed' else 1)
