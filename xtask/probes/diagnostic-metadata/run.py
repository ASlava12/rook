"""Owned actual CLI/daemon diagnostic metadata contract; retain all fixture state."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import tomllib
import urllib.error
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
parser.add_argument('--build-target',type=Path)
args=parser.parse_args()
assert __debug__ and not args.output.exists() and args.output.parent.is_dir()
source=Path(__file__).resolve().parents[3]
cargo=shutil.which('cargo')
suffix='.exe' if os.name=='nt' else ''
bins=args.bin_dir.resolve()
assert cargo and all((bins/(name+suffix)).is_file() for name in ['rook','rookd'])
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
root=Path(tempfile.mkdtemp(prefix='rook-diagnostic-metadata-',dir=parent)).resolve()
assert root.parent==parent
for name in ['helper','empty','logs']: (root/name).mkdir()
state=root/'state';home=state/'home';workspace=state/'workspace'

def sha(path):
    with path.open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()
binaries={name:{'path':str(bins/(name+suffix)),'sha256':sha(bins/(name+suffix))} for name in ['rook','rookd']}
inputs={name:sha(source/name) for name in ['Cargo.lock','xtask/probes/diagnostic-metadata/run.py','xtask/probes/diagnostic-metadata/seed.rs']}
report={'status':'incomplete','owned_directory':str(root),'binaries':binaries,'source_inputs':inputs}
requests=[];proxy_errors=[];base=None;daemon=None
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self,*unused): pass
    def do_POST(self):

        if len(requests)<128: requests.append({'method':'POST','path':self.path[:256]})
        self.send_error(500)
    def do_GET(self):
        if len(requests)<128: requests.append({'method':'GET','path':self.path[:256]})
        try:
            assert base and len(self.path)<=512
            try: response=opener.open(base+self.path,timeout=15)
            except urllib.error.HTTPError as error: response=error
            with response:
                data=response.read(2*1024*1024+1);assert len(data)<=2*1024*1024
                self.send_response(response.status)
                self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)))
                self.end_headers();self.wfile.write(data)
        except Exception as error:
            if len(proxy_errors)<8: proxy_errors.append(repr(error)[:500])
            self.send_error(502)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Proxy)
threading.Thread(target=server.serve_forever,daemon=True).start()
proxy='http://127.0.0.1:'+str(server.server_port)
env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(root/'empty'),ROOK_LOG='error')

def run(command,stem,timeout=45,child_env=None):
    stdout=root/'logs'/(stem+'.stdout');stderr=root/'logs'/(stem+'.stderr')
    with stdout.open('xb') as out,stderr.open('xb') as err:
        result=subprocess.run(command,env=child_env or env,cwd=workspace if workspace.exists() else root,
            stdin=subprocess.DEVNULL,stdout=out,stderr=err,timeout=timeout,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert stdout.stat().st_size<=2*1024*1024 and stderr.stat().st_size<=65536
    return result.returncode,stdout.read_bytes(),stderr.read_bytes()

def cli(identifier,stem):
    code,out,err=run([binaries['rook']['path'],'--workspace',str(workspace),'session','diagnostics',identifier],stem,child_env=dict(env,PATH=str(root/'empty')))
    return {'exit_code':code,'report':json.loads(out) if code==0 else None,'stderr':err.decode('utf-8',errors='replace')[-4000:]}

def http(identifier):
    try: response=opener.open(base+'/api/sessions/'+identifier+'/diagnostics?logs=false',timeout=15)
    except urllib.error.HTTPError as error: response=error
    with response:
        data=response.read(1024*1024+1);assert len(data)<=1024*1024
        return {'status':response.status,'report':json.loads(data)}

def neighbors(value,seed):
    if not isinstance(value,dict): return False
    seqs=[row.get('seq') for row in value.get('events',[])]
    return seed['before'] in seqs and seed['after'] in seqs and seed['oversized'] not in seqs

def omission(value): return isinstance(value,dict) and any('event metadata' in n for n in value.get('notices',[]))
def activity(value,seed):
    return isinstance(value,dict) and (value.get('activity') or {}).get('generation')==seed['generation'] and (value.get('activity') or {}).get('last_progress_at')==seed['last_progress_at']
def ancestor(value,seed):
    return isinstance(value,dict) and value.get('session',{}).get('id')==seed['child'] and value['session'].get('trace') is None and any('correlation is unavailable' in n for n in value.get('notices',[]))
try:
    project=root/'helper'
    shutil.copy2(Path(__file__).with_name('seed.rs'),project/'main.rs')
    shutil.copy2(source/'Cargo.lock',project/'Cargo.lock')
    manifest='[package]\nname="rook-diagnostic-metadata-seed"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\n'
    for name in ['rook-core','rook-store']: manifest+=name+'={path='+json.dumps(str(source/'crates'/name))+'}\n'
    manifest+='[[bin]]\nname="rook-diagnostic-metadata-seed"\npath="main.rs"\n'
    (project/'Cargo.toml').write_text(manifest,encoding='utf-8')
    target=(args.build_target or root/'build-target').resolve()
    build_env=dict(os.environ,CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='3',CARGO_PROFILE_DEV_DEBUG='0')
    print(json.dumps({'phase':'seed-helper-build'}),flush=True)
    code,out,err=run([cargo,'build','--manifest-path',str(project/'Cargo.toml'),'--target-dir',str(target),'--offline'],'seed-build',600,build_env)
    assert code==0,err[-3000:]
    original=tomllib.loads((source/'Cargo.lock').read_text(encoding='utf-8'))['package']
    allowed={(p['name'],p['version'],p.get('source'),p.get('checksum')) for p in original}
    resolved=tomllib.loads((project/'Cargo.lock').read_text(encoding='utf-8'))['package']
    assert all((p['name'],p['version'],p.get('source'),p.get('checksum')) in allowed for p in resolved if p['name']!='rook-diagnostic-metadata-seed')
    seed_binary=root/('seed'+suffix);shutil.copy2(target/'debug'/('rook-diagnostic-metadata-seed'+suffix),seed_binary)
    report['seed_binary_sha256']=sha(seed_binary)
    print(json.dumps({'phase':'seed-store'}),flush=True)
    code,out,err=run([str(seed_binary),'seed',str(state)],'seed-store')
    assert code==0,err[-3000:]
    seed=json.loads(out);report['seed']=seed
    (home/'config.toml').write_text("[agent]\nmodel='fixture'\ninstall_servers=false\n[models.fixture]\napi='openai'\nmodel='owned-diagnostic'\ncontext_window=32768\nurl='"+proxy+"/v1'\n",encoding='utf-8')
    before={p.relative_to(workspace).as_posix():sha(p) for p in workspace.rglob('*') if p.is_file()}
    print(json.dumps({'phase':'local-cli'}),flush=True)
    local={name:cli(seed[name],'local-'+name) for name in ['tail','primary','child','missing']}
    report['local']=local
    with (root/'logs/daemon.stdout').open('xb') as out,(root/'logs/daemon.stderr').open('xb') as err:
        daemon=subprocess.Popen([binaries['rookd']['path'],'--workspace',str(workspace),'--port','0'],env=dict(env,PATH=str(root/'empty')),cwd=workspace,
            stdin=subprocess.DEVNULL,stdout=out,stderr=err,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    deadline=time.monotonic()+30
    while True:
        assert daemon.poll() is None and time.monotonic()<deadline,'owned daemon failed startup'
        try:
            base=(home/'rookd.addr').read_text().strip()
            with opener.open(base+'/api/health',timeout=2) as response: json.load(response)
            break
        except (OSError,ValueError): time.sleep(.05)
    print(json.dumps({'phase':'owned-daemon-http'}),flush=True)
    native={name:http(seed[name]) for name in ['tail','primary','child']};report['http']=native
    # Only this owned address is routed through an observing loopback proxy.
    (home/'rookd.addr').write_text(proxy,encoding='utf-8')
    print(json.dumps({'phase':'remote-cli'}),flush=True)
    remote={name:cli(seed[name],'remote-'+name) for name in ['tail','primary','child']}
    report['remote_cli']=remote
    daemon.terminate();daemon.wait(timeout=20);daemon=None
    print(json.dumps({'phase':'saved-data-verification'}),flush=True)
    code,out,err=run([str(seed_binary),'verify',str(state),seed['tail'],seed['primary'],seed['parent']],'verify-store')
    saved=code==0 and json.loads(out).get('saved_metadata_unchanged') is True
    local_tail=local['tail']['report'];http_tail=native['tail']['report']
    report['checks']={
        'local_neighbors_preserved':neighbors(local_tail,seed),
        'local_omissions_reported':omission(local_tail),
        'local_activity_preserved':activity(local_tail,seed),
        'local_primary_metadata_refused':local['primary']['exit_code']!=0 and 'metadata' in local['primary']['stderr'] and 'private-title' not in local['primary']['stderr'],
        'local_ancestor_trace_omitted':ancestor(local['child']['report'],seed),
        'missing_session_still_refused':local['missing']['exit_code']!=0,
        'http_neighbors_preserved':native['tail']['status']==200 and neighbors(http_tail,seed),
        'http_omissions_reported':omission(http_tail),
        'http_activity_preserved':activity(http_tail,seed),
        'http_primary_metadata_refused':native['primary']['status']==500 and 'metadata' in json.dumps(native['primary']['report']) and 'private-title' not in json.dumps(native['primary']['report']),
        'http_ancestor_trace_omitted':native['child']['status']==200 and ancestor(native['child']['report'],seed),
        'remote_cli_never_lists_sessions':not any(r['path'].split('?')[0]=='/api/sessions' for r in requests) and any('/diagnostics' in r['path'] for r in requests),
        'remote_cli_matches_reader_contract':neighbors(remote['tail']['report'],seed) and ancestor(remote['child']['report'],seed) and remote['primary']['exit_code']!=0,
        'no_private_content_in_reports':all('private' not in json.dumps(v['report']) for v in [*local.values(),*native.values(),*remote.values()] if v.get('report') is not None),
        'no_model_http':not any(r['method']=='POST' or '/v1/' in r['path'] for r in requests),
        'saved_metadata_unchanged':saved,
        'workspace_unchanged':before=={p.relative_to(workspace).as_posix():sha(p) for p in workspace.rglob('*') if p.is_file()},
        'binaries_and_source_unchanged':all(sha(Path(v['path']))==v['sha256'] for v in binaries.values()) and all(sha(source/name)==digest for name,digest in inputs.items()),
    }
    assert len(report['checks'])==18 and not proxy_errors,proxy_errors
    report['status']='passed' if all(report['checks'].values()) else 'failed'
finally:
    if daemon is not None and daemon.poll() is None:
        daemon.terminate()
        try: daemon.wait(timeout=20)
        except subprocess.TimeoutExpired: daemon.kill();daemon.wait(timeout=10)
    server.shutdown();server.server_close()
    report['observed_http']=requests;report['proxy_errors']=proxy_errors
    with args.output.open('x',encoding='utf-8') as output: json.dump(report,output,indent=2)
    print(json.dumps({'status':report['status'],'checks':report.get('checks'),'owned_directory':str(root),'report':str(args.output)}),flush=True)
assert report['status']=='passed',report.get('checks')
