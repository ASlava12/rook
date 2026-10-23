"""Actual CLI/daemon local documentation and advisory Cargo hint contract; private seeded store and compiler-shaped helper."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
parser.add_argument('--source-dir',type=Path)
parser.add_argument('--build-target',type=Path)
args = parser.parse_args()
assert __debug__
output = args.output.resolve()
assert not output.exists() and output.parent.is_dir()
scripts = Path(__file__).resolve().parent
source=(args.source_dir or scripts.parents[2]).resolve()
shared=source/'xtask/probes/cargo-output'
bins = args.bin_dir.resolve()
compiler, node = shutil.which('rustc'), shutil.which('node')
assert compiler and node
suffix = '.exe' if os.name == 'nt' else ''
root = Path(tempfile.mkdtemp(prefix='rook-local-build-notes-', dir=args.tmp_root)).resolve()
for name in ['tools', 'logs', 'empty-skills']:
    (root / name).mkdir()

def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

binaries = {name: {'path': str(bins / (name + suffix)), 'sha256': sha(bins / (name + suffix))}
            for name in ['rook', 'rookd']}
input_paths={
    'Cargo.lock':source/'Cargo.lock',
    'xtask/probes/local-build-notes/run.py':scripts/'run.py',
    'xtask/probes/local-build-notes/seed.rs':scripts/'seed.rs',
    'xtask/probes/cargo-output/helper.rs':shared/'helper.rs',
    'xtask/probes/cargo-output/client.mjs':shared/'client.mjs',
}
inputs={name:sha(path) for name,path in input_paths.items()}
report = {'status': 'incomplete', 'owned_directory': str(root), 'binaries': binaries,
          'source_inputs': inputs, 'cases': []}
def spawn(command, name, env, cwd):
    child = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    captures = {'stdout': bytearray(), 'stderr': bytearray()}
    overflow = threading.Event()
    def drain(label, stream):
        while chunk := stream.read1(4096):
            room = 65536 - len(captures[label])
            captures[label].extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if child.poll() is None:
                    child.kill()
    readers = [threading.Thread(target=drain, args=(label, stream), daemon=True)
               for label, stream in [('stdout', child.stdout), ('stderr', child.stderr)]]
    for reader in readers:
        reader.start()
    return child, (name, captures, overflow, readers)

def finish(child, capture):
    name, captures, overflow, readers = capture
    if child.poll() is None:
        child.kill()
        child.wait(timeout=10)
    for reader in readers:
        reader.join(timeout=10)
    assert not any(reader.is_alive() for reader in readers)
    child.stdout.close()
    child.stderr.close()
    for label, data in captures.items():
        (root / 'logs' / (name + '.' + label)).write_bytes(data)
    assert not overflow.is_set(), 'owned child exceeded64KiB per stream'
    return child.returncode, bytes(captures['stdout']), bytes(captures['stderr'])

def run(command, name, env, cwd, timeout=120):
    child, capture = spawn(command, name, env, cwd)
    try:
        child.wait(timeout=timeout)
    finally:
        result = finish(child, capture)
    return result

records = {name: {'main':0,'auxiliary':0,'kinds':[],'catalog':False,'tools':[]} for name in ['cli','daemon']}
errors, paths, web_paths = [], [], []
lock=threading.Lock()
command=None

class Spy(http.server.BaseHTTPRequestHandler):
    def log_message(self,*unused):pass
    def do_GET(self):
        with lock:
            if len(web_paths)<32:web_paths.append(self.path[:512])
        body=b'{"results":[]}'
        self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)))
        self.end_headers();self.wfile.write(body)

def tool_content(content):
    try:value=json.loads(content)
    except (ValueError,TypeError):return content
    source=value.get('rook_source') if isinstance(value,dict) else None
    return source['content'] if isinstance(source,dict) and isinstance(source.get('content'),str) else content

class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):super().setup();self.connection.settimeout(10)
    def log_message(self,*unused):pass
    def do_GET(self):
        with lock:
            if len(errors)<8:errors.append('Unexpected model GET '+self.path[:512])
        self.send_error(500)
    def do_POST(self):
        try:
            size=int(self.headers.get('Content-Length','0'));assert 0<size<=2*1024*1024
            name=self.path.split('/')[1]
            assert name in records and self.path=='/'+name+'/v1/chat/completions'
            request=json.loads(self.rfile.read(size))
            classifier=any('Classify whether an assistant' in str(m.get('content','')) for m in request['messages'])
            verifier=any('checking a claim somebody else made' in str(m.get('content','')) for m in request['messages'])
            with lock:
                assert len(paths)<24;paths.append(self.path);record=records[name]
                if classifier or verifier:
                    record['auxiliary']+=1;record['kinds'].append('classifier' if classifier else 'verifier')
                    assert record['auxiliary']<=2;index=-1
                else:
                    record['main']+=1;index=record['main']-1;assert index<8
                    if index==0:
                        spec=next(t['function'] for t in request['tools'] if t['function']['name']=='docs')
                        record['catalog']=spec['parameters']['properties'].get('local_only',{}).get('type')=='boolean'
                    if index==7:
                        record['tools']=[{'id':m.get('tool_call_id'),'content':tool_content(m['content'])} for m in request['messages'] if m.get('role')=='tool']
            calls=[('run_command',{'command':command,'output':'auto'}),('run_command',{'command':command,'output':'auto'}),
                ('docs',{'topic':'rust errors','question':'E0502','local_only':True}),
                ('docs',{'topic':'redis','version':'7.4','question':'persistence','local_only':True}),
                ('docs',{'topic':'owned missing','local_only':True}),
                ('docs',{'topic':'oversized','local_only':True}),
                ('docs',{'topic':'rust errors','local_only':True,'refresh':True})]
            if 0<=index<7:
                tool,arguments=calls[index]
                message={'role':'assistant','tool_calls':[{'index':0,'id':'owned-'+str(index),'type':'function','function':{'name':tool,'arguments':json.dumps(arguments)}}]}
                ending='tool_calls'
            else:
                message={'role':'assistant','content':'{"action":"finish"}' if classifier else 'VERDICT: unproven' if verifier else 'OWNED_LOCAL_BUILD_DONE'}
                ending='stop'
            choice={'index':0,'finish_reason':ending}
            choice['delta' if request.get('stream') else 'message']=message
            value={'model':'owned-hints','choices':[choice],'usage':{'prompt_tokens':1,'completion_tokens':1}}
            body=(('data: '+json.dumps(value)+'\n\ndata: [DONE]\n\n') if request.get('stream') else json.dumps(value)).encode()
            self.send_response(200);self.send_header('Content-Type','text/event-stream' if request.get('stream') else 'application/json')
            self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
        except BaseException as error:
            with lock:
                if len(errors)<8:errors.append(type(error).__name__+': '+str(error)[:1000])
            self.close_connection=True

model=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model);model.daemon_threads=True
spy=http.server.ThreadingHTTPServer(('127.0.0.1',0),Spy);spy.daemon_threads=True
source_url=f'http://127.0.0.1:{spy.server_port}/owned-docs'
report['owned_source_url']=source_url
for server in [model,spy]:threading.Thread(target=server.serve_forever,daemon=True).start()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
daemon=capture=None
try:
    cargo=shutil.which('cargo');assert cargo
    import tomllib
    source=(args.source_dir or scripts.parents[2]).resolve()
    lock_sha=sha(source/'Cargo.lock');report['source_lock_sha256']=lock_sha;report['source_inputs']['Cargo.lock']=lock_sha
    env=dict(os.environ,PYTHONIOENCODING='utf-8',CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='3',CARGO_PROFILE_DEV_DEBUG='0')
    project=root/'seed-build';project.mkdir()
    shutil.copy2(scripts/'seed.rs',project/'main.rs');shutil.copy2(source/'Cargo.lock',project/'Cargo.lock')
    manifest='[package]\nname="owned-local-docs-seed"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nrook-store={path='+json.dumps(str(source/'crates/rook-store'))+'}\n[[bin]]\nname="owned-local-docs-seed"\npath="main.rs"\n'
    (project/'Cargo.toml').write_text(manifest,encoding='utf-8')
    target=(args.build_target or root/'seed-target').resolve()
    code,out,err=run([cargo,'build','--manifest-path',str(project/'Cargo.toml'),'--target-dir',str(target),'--offline'],'seed-build',env,root,600)
    assert code==0,err[-3000:]
    original=tomllib.loads((source/'Cargo.lock').read_text(encoding='utf-8'))['package']
    allowed={(p['name'],p['version'],p.get('source'),p.get('checksum')) for p in original}
    resolved=tomllib.loads((project/'Cargo.lock').read_text(encoding='utf-8'))['package']
    assert all((p['name'],p['version'],p.get('source'),p.get('checksum')) in allowed for p in resolved if p['name']!='owned-local-docs-seed')
    seed=root/('seed'+suffix);shutil.copy2(target/'debug'/('owned-local-docs-seed'+suffix),seed);seed_sha=sha(seed)
    helper=root/'tools'/('cargo'+suffix)
    code,out,err=run([compiler,'--edition','2024','-C','debuginfo=0',str(shared/'helper.rs'),'-o',str(helper)],'helper-build',env,root)
    assert code==0,err[-3000:]
    helper_sha=sha(helper)
    for program in ['rustc','python3','node','go','java','ruby','php','dotnet','git','npm','uv','docker','kubectl','rg','gh']:
        shutil.copy2(helper,root/'tools'/(program+suffix))
    command='"'+str(helper)+'" check --small'
    system_paths=str(Path(os.environ['SystemRoot'])/'System32') if os.name=='nt' else '/usr/bin:/bin'
    for name in ['cli','daemon']:
        print(json.dumps({'case':name}),flush=True)
        case=root/name;home=case/'home';workspace=case/'workspace';home.mkdir(parents=True);workspace.mkdir()
        (workspace/'sentinel.txt').write_bytes(b'Owned sentinel unchanged')
        env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(root/'empty-skills'),
            ROOK_LOG='error',ROOK_LOCAL_HINTS_KEY='owned-no-external-key',PATH=str(root/'tools')+os.pathsep+system_paths)
        code,out,err=run([str(seed),'seed',str(home),source_url],'seed-'+name,env,workspace)
        assert code==0 and b'OWNED_DOCS_SEED' in out,err[-2000:]
        (home/'config.toml').write_text(f"[agent]\nmodel='fixture'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=12\n[memory]\nenabled=false\n[sandbox]\nstance='autonomous'\nisolate='off'\nmax_output_bytes=8192\nmax_spill_bytes=1048576\n[shell_undo]\nenabled=true\n[web]\nenabled=true\nsearch='searxng'\nsearch_url='http://127.0.0.1:{spy.server_port}'\ntimeout_secs=1\n[server]\nport=0\n[models.fixture]\napi='openai'\nmodel='owned-hints'\nkey='env:ROOK_LOCAL_HINTS_KEY'\ncontext_window=131072\nurl='http://127.0.0.1:{model.server_port}/{name}/v1'\n",encoding='utf-8')
        for binary,record in binaries.items():
            code,out,err=run([record['path'],'--version'],'version-'+name+'-'+binary,env,workspace,30)
            assert code==0
            record.setdefault('version',out.decode().strip());assert record['version']==out.decode().strip()
        assert binaries['rook']['version'].removeprefix('rook ')==binaries['rookd']['version'].removeprefix('rookd ')
        marker='OWNED_LOCAL_BUILD_'+name.upper()
        if name=='cli':
            code,out,err=run([binaries['rook']['path'],'--workspace',str(workspace),'run',marker],name,env,workspace,150)
            client={'exit_code':code,'completed':code==0 and b'OWNED_LOCAL_BUILD_DONE' in out}
        else:
            daemon,capture=spawn([binaries['rookd']['path'],'--workspace',str(workspace),'--port','0'],'daemon',env,workspace)
            deadline=time.monotonic()+35
            while True:
                assert daemon.poll() is None and time.monotonic()<deadline
                try:
                    base=(home/'rookd.addr').read_text(encoding='utf-8').strip()
                    with opener.open(base+'/api/health',timeout=2) as response:assert response.status==200
                    break
                except (OSError,ValueError):time.sleep(.05)
            status=case/'client.json'
            code,out,err=run([node,str(shared/'client.mjs'),base,marker,str(status)],'websocket',env,workspace,150)
            client=json.loads(status.read_bytes());client.update(exit_code=code,completed=code==0 and client['done'] and client['stopped']=='end_turn')
            daemon.terminate();daemon.wait(timeout=10);finish(daemon,capture);daemon=capture=None
        code,out,err=run([str(seed),'verify',str(home),source_url],'verify-'+name,env,workspace)
        docs_unchanged=code==0 and b'OWNED_DOCS_VERIFY' in out
        with lock:record=json.loads(json.dumps(records[name]))
        results={r['id']:r['content'] for r in record['tools']}
        files={p.relative_to(workspace).as_posix():p.read_bytes() for p in workspace.rglob('*') if p.is_file()}
        workspace_ok=files=={'sentinel.txt':b'Owned sentinel unchanged','invoked.txt':b'check --small\ncheck --small\n'} and not any(p.is_dir() for p in workspace.rglob('*'))
        report['cases'].append({'name':name,'record':record,'tool_results':results,'client':client,'docs_unchanged':docs_unchanged,'workspace_ok':workspace_ok})
    cases={c['name']:c for c in report['cases']}
    def hints(name):
        c=cases[name];first=c['tool_results'].get('owned-0','');second=c['tool_results'].get('owned-1','')
        return c['client']['completed'] and 'OWNED_LOCAL_BUILD_HINT' in first and source_url in first and 'docs/rust-errors/latest' in first and 'Advisory local documentation:' not in second
    def hit(name):
        text=cases[name]['tool_results'].get('owned-2','')
        return 'OWNED_LOCAL_BUILD_HINT' in text and 'docs/rust-errors/latest' in text and 'No network request' in text
    def version(name):
        text=cases[name]['tool_results'].get('owned-3','')
        return 'OWNED_OLDER_DOC_SET' not in text and 'No matching admitted local documentation' in text and '(7.4)' in text
    report['checks']={
        'cli_hint_cited_and_deduplicated':hints('cli'),'daemon_hint_cited_and_deduplicated':hints('daemon'),
        'cli_local_hit_keeps_identity':hit('cli'),'daemon_local_hit_keeps_identity':hit('daemon'),
        'cli_wrong_version_neighbor_refused':version('cli'),'daemon_wrong_version_neighbor_refused':version('daemon'),
        'missing_docs_do_not_fetch':all('No network request' in c['tool_results'].get('owned-4','') and 'No matching admitted' in c['tool_results'].get('owned-4','') for c in report['cases']) and not web_paths,
        'oversized_docs_report_incomplete_with_bounded_reply':all('Local coverage is incomplete' in c['tool_results'].get('owned-5','') and len(c['tool_results'].get('owned-5','').encode())<=16384 for c in report['cases']),
        'refresh_conflict_refused_locally':all('local_only and refresh cannot be combined' in c['tool_results'].get('owned-6','') for c in report['cases']) and not web_paths,
        'local_mode_advertised':all(c['record']['catalog'] for c in report['cases']),
        'exact_model_request_budget':not errors and len(paths)==20 and all(c['record']['main']==8 and c['record']['auxiliary']==2 and sorted(c['record']['kinds'])==['classifier','verifier'] for c in report['cases']),
        'workspaces_and_doc_objects_unchanged':all(c['workspace_ok'] and c['docs_unchanged'] for c in report['cases']),
        'failed_exit_and_condensation_preserved':all(all('exit 101' in c['tool_results'].get('owned-'+str(i),'') and 'Cargo output condensed:' in c['tool_results'].get('owned-'+str(i),'') for i in [0,1]) for c in report['cases']),
        'binary_pair_unchanged':all(sha(b['path'])==b['sha256'] for b in binaries.values()),
        'fixture_and_helpers_unchanged':sha(helper)==helper_sha and sha(seed)==seed_sha and sha(source/'Cargo.lock')==lock_sha and all(sha(input_paths[p])==h for p,h in inputs.items()),
    }
    assert len(report['checks'])==15
    report['status']='passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure']=type(error).__name__+': '+str(error)[:2000]
    raise
finally:
    if daemon is not None:finish(daemon,capture)
    for server in [model,spy]:server.shutdown();server.server_close()
    report.update(model_paths=paths,web_paths=web_paths,observer_errors=errors)
    with output.open('x',encoding='utf-8') as sink:json.dump(report,sink,indent=2)
    print(json.dumps({'status':report['status'],'checks':report.get('checks'),'failure':report.get('failure'),'output':str(output)}),flush=True)
assert report['status']=='passed',report.get('checks')
