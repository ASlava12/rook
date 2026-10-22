"""Actual CLI discovery against five owned pending language-server probes."""
import argparse,hashlib,json,os,shutil,socket,socketserver,subprocess,tempfile,threading,time
from pathlib import Path

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
args=parser.parse_args()
suffix='.exe' if os.name=='nt' else ''
binary=args.bin_dir.resolve()/('rook'+suffix)
output=args.output.resolve()
compiler=shutil.which('rustc')
if not __debug__ or not binary.is_file() or not compiler or output.exists() or not output.parent.is_dir():
    parser.error('Use unoptimized Python, actual CLI, Rust compiler and a fresh output path')
compiler=os.path.abspath(compiler)
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
root=Path(tempfile.mkdtemp(prefix='rook-lsp-probe-pool-',dir=parent)).resolve()
assert root.parent==parent and root.name.startswith('rook-lsp-probe-pool-')
known=['rust-analyzer','typescript-language-server','pyright-langserver','gopls','clangd']
inventory=['rustc','python3','node','go','java','ruby','php','dotnet','git','cargo','npm','uv','docker','kubectl','rg','gh']

def sha(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()

def invoke(command,env,cwd,timeout):
    child=subprocess.Popen(command,env=env,cwd=cwd,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    captures={'stdout':bytearray(),'stderr':bytearray()};overflow=threading.Event()
    def drain(name,stream):
        while chunk:=stream.read1(4096):
            room=65536-len(captures[name]);captures[name].extend(chunk[:room])
            if len(chunk)>room:
                overflow.set()
                if child.poll() is None:child.kill()
    readers=[threading.Thread(target=drain,args=(name,stream),daemon=True) for name,stream in [('stdout',child.stdout),('stderr',child.stderr)]]
    for reader in readers:reader.start()
    try:code=child.wait(timeout=timeout)
    finally:
        if child.poll() is None:child.kill();child.wait(timeout=10)
        for reader in readers:reader.join(timeout=10)
        assert not any(reader.is_alive() for reader in readers),'owned capture reader did not retire'
        child.stdout.close();child.stderr.close()
    assert not overflow.is_set(),'owned command exceeded64KiB capture per stream'
    return code,bytes(captures['stdout']),bytes(captures['stderr'])

lock=threading.Lock()
state={'events':[],'errors':[],'active':0,'peak':0,'http_requests':0}
connections=[]
class Events(socketserver.StreamRequestHandler):
    def handle(self):
        live=False
        try:
            self.request.settimeout(65)
            with lock:
                assert len(connections)<16,'too many owned observer connections'
                connections.append(self.request)
            line=self.rfile.readline(257)
            assert len(line)<=256 and line.endswith(b'\n'),repr(line)
            if line.startswith((b'GET ',b'POST ',b'PUT ',b'PATCH ',b'DELETE ')):
                with lock:state['http_requests']+=1
                raise AssertionError('unexpected model HTTP')
            name,pid,eof,version=line.decode('ascii').split()
            assert name in known and pid.isdigit() and len(pid)<=10 and eof=='true' and version=='true'
            with lock:
                assert len(state['events'])<20
                state['active']+=1;state['peak']=max(state['peak'],state['active']);live=True
                state['events'].append({'phase':'start','tool':name,'pid':int(pid),'at_ns':time.monotonic_ns()})
            self.wfile.write(b'k');self.wfile.flush()
            try:assert self.rfile.read(1)==b''
            except ConnectionResetError:pass
            with lock:
                state['events'].append({'phase':'closed','tool':name,'pid':int(pid),'at_ns':time.monotonic_ns()})
                state['active']-=1;live=False
        except Exception as error:
            with lock:
                if len(state['errors'])<8:state['errors'].append(repr(error)[:500])
        finally:
            if live:
                with lock:state['active']-=1
class Server(socketserver.ThreadingTCPServer):
    daemon_threads=True
    request_queue_size=16

identity=sha(binary);source=Path(__file__).with_name('helper.rs').resolve();server=None
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'owned_directory':str(root),
    'script_sha256':sha(Path(__file__)),'helper_source_sha256':sha(source)}
try:
    for name in ['tools','home','workspace','empty-skills']:(root/name).mkdir()
    helper=root/'tools'/('rust-analyzer'+suffix)
    code,out,err=invoke([compiler,str(source),'--edition','2024','-C','opt-level=s','-C','debuginfo=0','-o',str(helper)],os.environ,root,90)
    assert code==0,(code,out[-2048:],err[-2048:])
    for name in known[1:]+inventory:
        path=root/'tools'/(name+suffix);shutil.copy2(helper,path);path.chmod(path.stat().st_mode|0o111)
    for name in ['main.rs','main.ts','main.py','main.go','main.c']:(root/'workspace'/name).write_bytes(b'owned mixed workspace')
    before={p.name:sha(p) for p in (root/'workspace').iterdir()}
    server=Server(('127.0.0.1',0),Events)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    (root/'home/config.toml').write_text(f"[agent]\nmodel='owned'\ninstall_servers=false\n[models.owned]\napi='openai'\nmodel='unused'\nurl='http://127.0.0.1:{server.server_address[1]}/v1'\n",encoding='utf-8')
    env=dict(os.environ,ROOK_HOME=str(root/'home'),ROOK_CONFIG_DIR=str(root/'home'),ROOK_BUILTIN_SKILLS=str(root/'empty-skills'),PATH=str(root/'tools'),ROOK_LOG='error',ROOK_OWNED_PARALLEL_LSP_ADDR='127.0.0.1:'+str(server.server_address[1]))
    started=time.monotonic()
    code,out,err=invoke([str(binary),'--workspace',str(root/'workspace'),'lsp','servers'],env,root/'workspace',90)
    elapsed=time.monotonic()-started
    until=time.monotonic()+3
    while time.monotonic()<until:
        with lock:
            if state['active']==0:break
        time.sleep(.01)
    with lock:measurement=json.loads(json.dumps(state))
    starts=[e for e in measurement['events'] if e['phase']=='start'];closed=[e for e in measurement['events'] if e['phase']=='closed']
    report.update(exit_code=code,seconds=elapsed,stdout=out.decode('utf-8',errors='replace'),stderr=err.decode('utf-8',errors='replace'),measurements=measurement)
    report['checks']={
        'pending_probes_overlap':measurement['peak']>1,
        'four_probe_cap_preserved':1<=measurement['peak']<=4,
        'exact_five_version_probes_once':len(starts)==5 and sorted(e['tool'] for e in starts)==sorted(known) and not measurement['errors'],
        'all_connections_retired_before_fixture_cleanup':measurement['active']==0 and len(closed)==5,
        'known_language_output_order_preserved':code==0 and out.decode('utf-8').strip()=='rust, typescript, python, go, c',
        'no_model_http':measurement['http_requests']==0,
        'workspace_unchanged':before=={p.name:sha(p) for p in (root/'workspace').iterdir()},
        'binary_unchanged':sha(binary)==identity,
    }
    assert len(report['checks'])==8
    report['status']='passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure']=repr(error)[:1000]
    raise
finally:
    with output.open('x',encoding='utf-8') as sink:json.dump(report,sink,indent=2)
    if server is not None:server.shutdown();server.server_close()
    for connection in connections:
        try:connection.shutdown(socket.SHUT_RDWR)
        except OSError:pass
        connection.close()
    print(json.dumps(report),flush=True)
assert report['status']=='passed',report.get('checks')
