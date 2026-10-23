"""Real CLI/daemon crate_api calls against private, digest-bound Cargo caches."""
import argparse, gzip, hashlib, http.server, io, json, os, shutil, subprocess, tarfile, tempfile, threading, time, urllib.request
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
args = parser.parse_args()
assert __debug__ and not args.output.exists() and args.output.parent.is_dir()
scripts = Path(__file__).resolve().parent
compiler,node = shutil.which('rustc'),shutil.which('node')
assert compiler and node
root = Path(tempfile.mkdtemp(prefix='rook-crate-identity-',dir=args.tmp_root)).resolve()
for name in ['logs','tools','empty-skills']: (root/name).mkdir()
suffix = '.exe' if os.name == 'nt' else ''
def sha(path):
    with Path(path).open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()
binaries = {n:{'path':str(args.bin_dir.resolve()/(n+suffix)),'sha256':sha(args.bin_dir.resolve()/(n+suffix))} for n in ['rook','rookd']}
inputs = {n:sha(scripts/n) for n in ['run.py','client.mjs','version.rs']}
report = {'status':'incomplete','owned_directory':str(root),'binaries':binaries,'source_inputs':inputs,'cases':[]}

def spawn(command,env,cwd,name):
    child = subprocess.Popen(command,env=env,cwd=cwd,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    buffers = {'stdout':bytearray(),'stderr':bytearray()}; overflow = threading.Event()
    def drain(label,stream):
        while chunk := stream.read1(4096):
            room = 65536-len(buffers[label]); buffers[label].extend(chunk[:room])
            if len(chunk)>room:
                overflow.set()
                if child.poll() is None: child.kill()
    readers=[threading.Thread(target=drain,args=(label,stream),daemon=True) for label,stream in [('stdout',child.stdout),('stderr',child.stderr)]]
    for reader in readers: reader.start()
    return child,(name,buffers,overflow,readers)
def finish(child,capture):
    if child.poll() is None: child.kill(); child.wait(timeout=10)
    name,buffers,overflow,readers=capture
    for reader in readers: reader.join(timeout=10)
    assert not any(r.is_alive() for r in readers)
    child.stdout.close(); child.stderr.close()
    for label,data in buffers.items(): (root/'logs'/(name+'.'+label)).write_bytes(data)
    assert not overflow.is_set(),'owned process capture limit'
    return child.returncode,bytes(buffers['stdout']),bytes(buffers['stderr'])
def run(command,env,cwd,name,timeout=120):
    child,capture=spawn(command,env,cwd,name)
    try: child.wait(timeout=timeout)
    finally: result=finish(child,capture)
    return result
def snapshot(directory):
    result={};total=0;pending=[directory]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                assert len(result)<64
                p=Path(entry.path);key=p.relative_to(directory).as_posix()
                # Include Windows junctions on Python versions before is_junction.
                reparse=bool(getattr(p.lstat(),'st_file_attributes',0)&0x400)
                if p.is_symlink() or reparse: result[key]={'link':os.readlink(p)}
                elif entry.is_dir(follow_symlinks=False): result[key]={'directory':True};pending.append(p)
                else:
                    total+=p.stat().st_size;assert total<=1024*1024
                    result[key]={'sha256':sha(p)}
    return result
def seed(cache,registry,version,marker,name='owned-example'):
    package=name+'-'+version
    body=('pub struct '+marker+';\n').encode()
    raw=io.BytesIO()
    with tarfile.open(fileobj=raw,mode='w') as archive:
        info=tarfile.TarInfo(package+'/src/lib.rs');info.size=len(body);archive.addfile(info,io.BytesIO(body))
    content=gzip.compress(raw.getvalue(),mtime=0)
    archive=cache/'registry/cache'/registry/(package+'.crate');archive.parent.mkdir(parents=True,exist_ok=True);archive.write_bytes(content)
    source=cache/'registry/src'/registry/package/'src/lib.rs';source.parent.mkdir(parents=True,exist_ok=True);source.write_bytes(body)
    return hashlib.sha256(content).hexdigest()
records={n:{'main':0,'classifier':0,'verifier':0,'tools':[],'catalog':False} for n in ['cli','daemon']}
paths,errors=[],[];lock=threading.Lock()
def content(text):
    try: data=json.loads(text)
    except (ValueError,TypeError): return text
    source=data.get('rook_source') if isinstance(data,dict) else None
    return source['content'] if isinstance(source,dict) and isinstance(source.get('content'),str) else text
class Model(http.server.BaseHTTPRequestHandler):
    def setup(self): super().setup();self.connection.settimeout(10)
    def log_message(self,*unused): pass
    def do_POST(self):
        try:
            size=int(self.headers.get('Content-Length','0'));assert 0<size<=2*1024*1024 and len(self.path)<=512
            name=self.path.split('/')[1];assert name in records and self.path=='/'+name+'/v1/chat/completions'
            request=json.loads(self.rfile.read(size));classifier=any('Classify whether an assistant' in str(m.get('content','')) for m in request['messages'])
            verifier=any('checking a claim somebody else made' in str(m.get('content','')) for m in request['messages'])
            with lock:
                assert len(paths)<12;paths.append(self.path);record=records[name]
                if classifier or verifier:
                    kind='classifier' if classifier else 'verifier'
                    record[kind]+=1;assert record[kind]==1;index=-1
                else:
                    index=record['main'];record['main']+=1;assert index<4
                    if index==0:
                        spec=next(t['function'] for t in request['tools'] if t['function']['name']=='crate_api')
                        record['catalog']=spec['parameters']['properties'].get('version',{}).get('type')=='string'
                    if index==3: record['tools']=[{'id':m.get('tool_call_id'),'content':content(m['content'])} for m in request['messages'] if m.get('role')=='tool']
            if index in [0,1,2]:
                arguments={'crate':'owned-linked' if index==2 else 'owned-example'}
                if index==1: arguments['version']='2.0.0'
                delta={'role':'assistant','tool_calls':[{'index':0,'id':'owned-'+str(index),'type':'function','function':{'name':'crate_api','arguments':json.dumps(arguments)}}]};ending='tool_calls'
            else: delta={'role':'assistant','content':'{"action":"finish"}' if classifier else 'VERDICT: unproven' if verifier else 'OWNED_CRATE_IDENTITY_DONE'};ending='stop'
            message={'model':'owned-crate','choices':[{'index':0,('delta' if request.get('stream') else 'message'):delta,'finish_reason':ending}],'usage':{'prompt_tokens':1,'completion_tokens':1}}
            data=(('data: '+json.dumps(message)+'\n\ndata: [DONE]\n\n') if request.get('stream') else json.dumps(message)).encode()
            self.send_response(200);self.send_header('Content-Type','text/event-stream' if request.get('stream') else 'application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except BaseException as error:
            with lock:
                if len(errors)<8: errors.append(type(error).__name__+': '+str(error)[:1000])
            self.close_connection=True
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model);server.daemon_threads=True
threading.Thread(target=server.serve_forever,daemon=True).start()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
daemon=capture=None
try:
    helper=root/'tools'/('cargo'+suffix)
    code,out,err=run([compiler,'--edition','2024',str(scripts/'version.rs'),'-o',str(helper)],dict(os.environ),root,'helper-build')
    assert code==0,err[-2000:]
    helper_hash=sha(helper)
    for name in ['rustc','python3','node','go','java','ruby','php','dotnet','git','npm','uv','docker','kubectl','rg','gh']: shutil.copy2(helper,root/'tools'/(name+suffix))
    for name in ['cli','daemon']:
        print(json.dumps({'case':name}),flush=True)
        case=root/name;home=case/'home';workspace=case/'workspace';cache=case/'cargo-home'
        home.mkdir(parents=True);workspace.mkdir()
        one=seed(cache,'registry-a','1.0.0','WrongVersion')
        seed(cache,'registry-a','2.0.0','WrongSource')
        two=seed(cache,'registry-b','2.0.0','RightVersion')
        (workspace/'Cargo.lock').write_text('version = 4\n'+''.join('\n[[package]]\nname = "owned-example"\nversion = "'+v+'"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "'+c+'"\n' for v,c in [('1.0.0',one),('2.0.0',two)]),encoding='utf-8')
        linked=seed(cache,'registry-link','1.0.0','UnlinkedOriginal','owned-linked')
        with (workspace/'Cargo.lock').open('a',encoding='utf-8') as sink: sink.write('\n[[package]]\nname = "owned-linked"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "'+linked+'"\n')
        linked_root=cache/'registry/src/registry-link/owned-linked-1.0.0/src'
        (linked_root/'lib.rs').unlink();linked_root.rmdir()
        outside=case/'outside-cache';outside.mkdir();(outside/'lib.rs').write_text('pub struct OutsideCacheRoot;\n',encoding='utf-8')
        if os.name=='nt':
            result=subprocess.run(['cmd.exe','/d','/c','mklink','/J',str(linked_root),str(outside)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=20,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            assert result.returncode==0,'owned source junction creation failed'
        else: linked_root.symlink_to(outside,target_is_directory=True)
        (workspace/'Cargo.toml').write_text('[package]\nname="owned-project"\nversion="0.1.0"\n[dependencies]\nowned-example="=2.0.0"\n',encoding='utf-8')
        before={'workspace':snapshot(workspace),'cache':snapshot(cache),'outside':snapshot(outside)}
        system=str(Path(os.environ['SystemRoot'])/'System32') if os.name=='nt' else '/usr/bin:/bin'
        env=dict(os.environ,CARGO_HOME=str(cache),ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(root/'empty-skills'),ROOK_CRATE_KEY='owned',ROOK_LOG='error',PATH=str(root/'tools')+os.pathsep+system)
        (home/'config.toml').write_text("[agent]\nmodel='fixture'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=4\n[memory]\nenabled=false\n[sandbox]\nstance='autonomous'\nisolate='off'\n[server]\nport=0\n[models.fixture]\napi='openai'\nmodel='owned-crate'\nkey='env:ROOK_CRATE_KEY'\ncontext_window=131072\nurl='http://127.0.0.1:"+str(server.server_port)+"/"+name+"/v1'\n",encoding='utf-8')
        if name=='cli':
            code,out,err=run([binaries['rook']['path'],'--workspace',str(workspace),'run','OWNED_CRATE_IDENTITY'],env,workspace,'cli')
            completed=code==0 and b'OWNED_CRATE_IDENTITY_DONE' in out
        else:
            daemon,capture=spawn([binaries['rookd']['path'],'--workspace',str(workspace),'--port','0'],env,workspace,'daemon')
            deadline=time.monotonic()+35
            while True:
                assert daemon.poll() is None and time.monotonic()<deadline,'owned daemon startup'
                try:
                    base=(home/'rookd.addr').read_text(encoding='utf-8').strip()
                    with opener.open(base+'/api/health',timeout=2) as response: assert response.status==200
                    break
                except (OSError,ValueError): time.sleep(0.05)
            status=case/'client.json'
            code,out,err=run([node,str(scripts/'client.mjs'),base,'OWNED_CRATE_IDENTITY',str(status)],env,workspace,'websocket')
            client=json.loads(status.read_bytes());completed=code==0 and client['started'] and client['done'] and client['stopped']=='end_turn'
            daemon.terminate();daemon.wait(timeout=10);finish(daemon,capture);daemon=capture=None
        with lock: record=json.loads(json.dumps(records[name]))
        tools={t['id']:t['content'] for t in record['tools']}
        report['cases'].append({'name':name,'completed':completed,'record':record,'tools':tools,'unchanged':before=={'workspace':snapshot(workspace),'cache':snapshot(cache),'outside':snapshot(outside)}})
    cases={c['name']:c for c in report['cases']}
    report['checks']={
        'cli_ambiguity_refused':'Multiple locked versions' in cases['cli']['tools'].get('owned-0',''),
        'daemon_ambiguity_refused':'Multiple locked versions' in cases['daemon']['tools'].get('owned-0',''),
        'cli_exact_locked_version':'RightVersion' in cases['cli']['tools'].get('owned-1',''),
        'daemon_exact_locked_version':'RightVersion' in cases['daemon']['tools'].get('owned-1',''),
        'cli_source_root_link_refused':'Source scan incomplete' in cases['cli']['tools'].get('owned-2','') and 'OutsideCacheRoot' not in cases['cli']['tools'].get('owned-2',''),
        'daemon_source_root_link_refused':'Source scan incomplete' in cases['daemon']['tools'].get('owned-2','') and 'OutsideCacheRoot' not in cases['daemon']['tools'].get('owned-2',''),
        'wrong_cached_source_ignored':all('WrongSource' not in c['tools'].get('owned-1','') and 'WrongVersion' not in c['tools'].get('owned-1','') for c in report['cases']),
        'version_argument_advertised':all(c['record']['catalog'] for c in report['cases']),
        'normal_completion_both':all(c['completed'] for c in report['cases']),
        'exact_bounded_model_requests':not errors and len(paths)==12 and all(c['record']['main']==4 and c['record']['classifier']==1 and c['record']['verifier']==1 and len(c['tools'])==3 for c in report['cases']),
        'workspaces_and_caches_unchanged':all(c['unchanged'] for c in report['cases']),
        'binaries_and_fixture_unchanged':all(sha(b['path'])==b['sha256'] for b in binaries.values()) and sha(helper)==helper_hash and all(sha(scripts/n)==v for n,v in inputs.items()),
    }
    assert len(report['checks'])==12
    report['status']='passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure']=type(error).__name__+': '+str(error)[:2000]
    raise
finally:
    if daemon is not None: finish(daemon,capture)
    server.shutdown();server.server_close();report['paths']=paths;report['errors']=errors
    with args.output.open('x',encoding='utf-8') as sink: json.dump(report,sink,indent=2)
    print(json.dumps({'status':report['status'],'checks':report.get('checks'),'failure':report.get('failure'),'output':str(args.output)}),flush=True)
assert report['status']=='passed',report.get('checks')
