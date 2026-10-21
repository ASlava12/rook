"""Owned daemon background cache installation; no system package manager."""
import argparse
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.request
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__ or args.output.exists() or not args.output.parent.is_dir():
    parser.error('Unoptimized Python and a fresh output path are required.')
binary = args.bin_dir.resolve()/('rookd.exe' if os.name=='nt' else 'rookd')
assert binary.is_file()
node = shutil.which('node')
assert node
labels = ['NATIVE_INSTALL_A', 'NATIVE_INSTALL_B']
model_requests = dict.fromkeys(labels, 0)
api_requests = 0
downloads = 0
errors = []
counts_lock = threading.Lock()
gate = threading.Event()
payload = b'owned language server fixture; no executable program\n'
packed = io.BytesIO()
with zipfile.ZipFile(packed, 'w', compression=zipfile.ZIP_STORED) as archive:
    archive.writestr('rust-analyzer.exe', payload)
archives = {'.zip': packed.getvalue(), '.gz': gzip.compress(payload)}

class Service(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)
    def log_message(self, *unused):
        pass
    def reply(self, body, kind='application/json', status=200):
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        global api_requests, downloads
        try:
            if self.path.endswith('/releases/latest'):
                with counts_lock:
                    api_requests += 1
                    assert api_requests <= 4
                assets = []
                for arch in ['x86_64', 'aarch64']:
                    for target, suffix in [('pc-windows-msvc', '.zip'), ('apple-darwin', '.gz'), ('unknown-linux-gnu', '.gz'), ('unknown-linux-musl', '.gz')]:
                        name = f'rust-analyzer-{arch}-{target}{suffix}'
                        data = archives[suffix]
                        assets.append({'name':name, 'size':len(data), 'digest':'sha256:'+hashlib.sha256(data).hexdigest(),
                            'browser_download_url':f'http://127.0.0.1:{self.server.server_port}/asset/{name}'})
                self.reply(json.dumps({'tag_name':'owned-native-v1','assets':assets}).encode())
            elif self.path.startswith('/asset/'):
                with counts_lock:
                    downloads += 1
                    assert downloads <= 4
                assert gate.wait(75), 'owned asset gate deadline'
                suffix = '.zip' if self.path.endswith('.zip') else '.gz'
                self.reply(archives[suffix], 'application/octet-stream')
            else:
                assert self.path.endswith('/models')
                self.reply(b'{"data":[{"id":"fixture"}]}')
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            with counts_lock:
                if len(errors) < 16: errors.append(str(error)[:1000])
            self.reply(b'{}', status=500)
    def do_POST(self):
        try:
            size = int(self.headers.get('content-length', 0))
            assert 0 < size <= 2*1024*1024
            request = json.loads(self.rfile.read(size))
            users = '\n'.join(str(m.get('content','')) for m in request['messages'] if m['role']=='user')
            label = next(label for label in labels if label in users)
            with counts_lock:
                model_requests[label] += 1
                assert model_requests[label] <= 2
            if request.get('stream'):
                chunk = {'model':'fixture','choices':[{'index':0,'delta':{'content':'Inspection complete.'},'finish_reason':'stop'}],
                         'usage':{'prompt_tokens':10,'completion_tokens':5}}
                self.reply(('data: '+json.dumps(chunk)+'\n\ndata: [DONE]\n\n').encode(), 'text/event-stream')
            else:
                self.reply(json.dumps({'model':'fixture','choices':[{'message':{'role':'assistant','content':'{"action":"finish"}'},'finish_reason':'stop'}],
                           'usage':{'prompt_tokens':1,'completion_tokens':1}}).encode())
        except Exception as error:
            with counts_lock:
                if len(errors) < 16: errors.append(str(error)[:1000])
            self.reply(b'{}', status=500)

server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Service)
threading.Thread(target=server.serve_forever, daemon=True).start()
daemon = None
clients = []
temporary = tempfile.TemporaryDirectory(prefix='rook-install-native-')
root = Path(temporary.name)
try:
    home = root/'home'; home.mkdir()
    empty = root/'empty'; empty.mkdir()
    workspaces = []
    for label in labels:
        workspace = root/label; workspace.mkdir()
        (workspace/'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\nedition="2024"\n',encoding='utf-8')
        (workspace/'lib.rs').write_text('// owned fixture\n',encoding='utf-8')
        workspaces.append(workspace)
    (home/'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=true
plan_first=false
one_script=false
max_steps=3
[sandbox]
stance='autonomous'
[work]
max_parallel_runs=2
[server]
port=0
[models.fixture]
model='fixture'
api='openai'
parallel=4
url='http://127.0.0.1:{server.server_port}/v1'
context_window=32768
[[lsp]]
language='disabled-fixture'
command='unused'
enabled=false
extensions=[]
''',encoding='utf-8')
    env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_BUILTIN_SKILLS=str(empty),
               ROOK_RELEASE_API=f'http://127.0.0.1:{server.server_port}', ROOK_LOG='error', PATH=str(empty))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def get(url):
        with opener.open(url,timeout=10) as response: data=response.read(512*1024+1)
        assert len(data)<=512*1024
        return json.loads(data)
    def until(check, label):
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            assert not errors, errors
            if daemon is not None: assert daemon.poll() is None, 'owned daemon exited'
            result=check()
            if result: return result
            for process in clients:
                assert process.poll() in (None,0), 'owned chat client failed'
            time.sleep(.025)
        raise AssertionError(label)
    daemon_log=root/'daemon.log'
    with daemon_log.open('wb') as sink:
        daemon=subprocess.Popen([str(binary),'--workspace',str(workspaces[0]),'--port','0'],env=env,cwd=workspaces[0],
                                stdout=sink,stderr=subprocess.STDOUT)
    def ready():
        try:
            value=(home/'rookd.addr').read_text().strip();get(value+'/api/health');return value
        except (OSError,ValueError):return None
    base=until(ready,'owned daemon startup')
    outputs={}
    def launch(index):
        label=labels[index]; outputs[label]=root/(label+'.json')
        clients.append(subprocess.Popen([node,str(Path(__file__).with_name('client.mjs')),base,str(workspaces[index]),label,str(outputs[label])],
                                        env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL))
    def status(label):
        try:return json.loads(outputs[label].read_text())
        except (OSError,ValueError):return {}
    launch(0)
    until(lambda:downloads>=1,'first physical cache download')
    launch(1)
    until(lambda:all(model_requests[label]==2 and status(label).get('session') for label in labels),'both model turns before install release')
    sessions={label:status(label)['session'] for label in labels}
    before={label:get(base+'/api/sessions/'+sessions[label]+'/transcript?limit=100&max_body=4096')['items'] for label in labels}
    listing={row['id']:row for row in get(base+'/api/sessions')['items']}
    views={label:listing[sessions[label]] for label in labels}
    # Both loops have durable install intents and have yielded through two model
    # requests. Retain a bounded observation window before releasing the asset.
    observed_until=time.monotonic()+1
    while time.monotonic()<observed_until:
        assert not errors, errors
        time.sleep(.025)
    before_effects=all(not any(e['label']=='harness:lsp install' and e['body']=='installed' for e in before[label]) for label in labels)
    gate.set()
    until(lambda:all(process.poll() is not None for process in clients),'ordinary chats complete')
    statuses={label:status(label) for label in labels}
    transcripts={label:get(base+'/api/sessions/'+sessions[label]+'/transcript?limit=100&max_body=4096')['items'] for label in labels}
    recovery={label:get(base+'/api/sessions/'+sessions[label]+'/recovery') for label in labels}
    current=home/'servers'/'rust-analyzer'/'current'
    cached=current/('rust-analyzer.exe' if os.name=='nt' else 'rust-analyzer')
    checks={
        'distinct_workspace_sessions':len(set(sessions.values()))==2 and all(Path(views[label]['workspace']).samefile(workspace) for label,workspace in zip(labels,workspaces)),
        'models_before_install_effects':before_effects and all(sum(e['label']=='harness:lsp install' and e['body']=='rust-analyzer' for e in before[label])==1 for label in labels),
        'one_metadata_lookup':api_requests==1,
        'one_physical_download':downloads==1,
        'both_chats_finish':all(s['done'] and s['stopped']=='end_turn' and not s['error'] for s in statuses.values()),
        'each_background_receipt_finished':all(sum(e['label']=='harness:lsp install' and e['body']=='installed' for e in transcripts[label])==1 for label in labels),
        'one_checked_cached_payload':cached.is_file() and cached.read_bytes()==payload and (current/'.tag').read_text()=='owned-native-v1',
        'no_unknown_or_pending_effects':all(recovery[label] and all(not row['unknown'] and not row['background'] and row['pending'] is None for row in recovery[label]) for label in labels),
        'exact_model_requests':all(model_requests[label]==2 for label in labels),
        'cache_fetch_only':all(not any(e['label']=='harness:system install' for e in transcripts[label]) for label in labels),
    }
    with binary.open('rb') as executable:digest=hashlib.file_digest(executable,'sha256').hexdigest()
    report={'checks':checks,'binary':str(binary),'binary_sha256':digest,'api_requests':api_requests,'asset_requests':downloads,
            'model_requests':model_requests,'statuses':statuses,
            'workspaces':{label:{'recorded':views[label]['workspace'],'requested':str(workspace)} for label,workspace in zip(labels,workspaces)},
            'install_receipts':{label:[{'label':e['label'],'body':e['body'][:1500]} for e in transcripts[label] if e['label'] in ['harness:lsp install','lsp install']] for label in labels},
            'scope':'One owned daemon, two ordinary workspace chats, one temporary cache and local verified fake release; no package manager or language server execution.'}
    with args.output.open('x',encoding='utf-8') as sink:json.dump(report,sink,indent=2)
    print(json.dumps(report),flush=True)
    assert all(checks.values()),report
except BaseException:
    if 'daemon_log' in globals() and daemon_log.exists(): print(daemon_log.read_text(encoding='utf-8',errors='replace')[-8000:],flush=True)
    raise
finally:
    gate.set()
    for process in clients:
        if process.poll() is None:
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
    if daemon is not None and daemon.poll() is None:
        daemon.terminate()
        try:daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:daemon.kill();daemon.wait(timeout=5)
    server.shutdown();server.server_close();temporary.cleanup()
