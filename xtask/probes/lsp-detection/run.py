"""Actual CLI PATH discovery against owned replies, refusals and descendants."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socketserver
import subprocess
import tempfile
import threading
import time

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
    parser.error('Unoptimized Python, CLI, compiler and fresh output path required')
compiler=os.path.abspath(compiler)
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
root=Path(tempfile.mkdtemp(prefix='rook-lsp-detection-',dir=parent)).resolve()
assert root.parent==parent and root.name.startswith('rook-lsp-detection-')
modes=['success','refusal','pending','fork-success','fork-refusal']
inventory=['rustc','python3','node','go','java','ruby','php','dotnet','git','cargo','npm','uv','docker','kubectl','rg','gh']
lock=threading.Lock()
records=[]
connections=[]
errors=[]
http_requests=[]

def sha(path):
    with path.open('rb') as source: return hashlib.file_digest(source,'sha256').hexdigest()

class Events(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(40)
        try:
            line=self.rfile.readline(257)
            assert len(line)<=256 and line.endswith(b'\n')
            text=line.decode('ascii').strip()
            if text.startswith(('GET ','POST ','PUT ','PATCH ','DELETE ')):
                with lock: http_requests.append(text[:200])
                return
            phase,mode,pid,eof=text.split()
            assert phase in ['start','holder'] and mode in modes and pid.isdigit() and eof in ['true','false']
            record={'phase':phase,'mode':mode,'pid':int(pid),'stdin_eof':eof=='true','connected_ns':time.monotonic_ns()}
            with lock:
                assert len(records)<10
                records.append(record)
                connections.append((mode,self.request))
            self.wfile.write(b'k');self.wfile.flush()
            try: assert self.rfile.read(1)==b''
            except ConnectionResetError: pass
            with lock: record['closed_ns']=time.monotonic_ns()
        except Exception as error:
            with lock:
                if len(errors)<8: errors.append(repr(error)[:400])

class Server(socketserver.ThreadingTCPServer):
    daemon_threads=True
    request_queue_size=16

server=Server(('127.0.0.1',0),Events)
threading.Thread(target=server.serve_forever,daemon=True).start()
identity=sha(binary)
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'owned_directory':str(root),'measurements':{'events':records,'errors':errors,'cases':[]}}
successful=False

def invoke(command,env,cwd,stem,timeout,limit):
    # Files avoid confusing process completion with an inherited pipe's EOF.
    out=root/(stem+'.stdout');err=root/(stem+'.stderr')
    with out.open('xb') as stdout,err.open('xb') as stderr:
        result=subprocess.run(command,env=env,cwd=cwd,stdin=subprocess.DEVNULL,
            stdout=stdout,stderr=stderr,timeout=timeout,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert out.stat().st_size<=limit and err.stat().st_size<=limit
    return result.returncode,out.read_bytes(),err.read_bytes()

try:
    for name in ['tools','empty']: (root/name).mkdir()
    helper=root/'tools'/('rust-analyzer'+suffix)
    source=Path(__file__).with_name('helper.rs')
    code,out,err=invoke([compiler,str(source),'--edition','2024','-C','opt-level=s','-C','debuginfo=0','-o',str(helper)],
        os.environ,root,'compiler',90,65536)
    assert code==0,(out[-2000:],err[-2000:])
    for name in inventory:
        path=root/'tools'/(name+suffix)
        shutil.copy2(helper,path);path.chmod(path.stat().st_mode|0o111)
    cases=[]
    for mode in modes:
        case=root/mode;case.mkdir()
        home=case/'home';home.mkdir()
        workspace=case/'workspace';workspace.mkdir()
        sentinel=workspace/'main.rs';sentinel.write_bytes(b'fn main() {}\n')
        before=sha(sentinel)
        (home/'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
[models.fixture]
api='openai'
model='owned-lsp-detection'
context_window=32768
url='http://127.0.0.1:{server.server_address[1]}/v1'
''',encoding='utf-8')
        env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),
            ROOK_BUILTIN_SKILLS=str(root/'empty'),PATH=str(root/'tools'),ROOK_LOG='error',
            ROOK_OWNED_LSP_MODE=mode,ROOK_OWNED_LSP_ENDPOINT=f'127.0.0.1:{server.server_address[1]}',
            ROOK_OWNED_LSP_MARKER=str(case/'holder.ready'))
        started=time.monotonic()
        code,out,err=invoke([str(binary),'--workspace',str(workspace),'lsp','servers'],env,workspace,mode,30,65536)
        ended=time.monotonic_ns()
        until=time.monotonic()+2
        while time.monotonic()<until:
            with lock:
                active=[row for row in records if row['mode']==mode and 'closed_ns' not in row]
            if not active: break
            time.sleep(.01)
        with lock:
            events=[dict(row) for row in records if row['mode']==mode]
            sockets=[socket for name,socket in connections if name==mode]
        error_text=err.decode('utf-8',errors='replace')
        error_preview=error_text if len(error_text)<=2000 else error_text[:1000]+'\n[trace middle omitted]\n'+error_text[-1000:]
        entry={'mode':mode,'exit_code':code,'stdout':out.decode('utf-8',errors='replace'),
            'stderr':error_preview,'seconds':time.monotonic()-started,
            'cli_ended_ns':ended,'events_before_fixture_cleanup':events,
            'all_connections_retired_before_fixture_cleanup':bool(events) and all('closed_ns' in row for row in events),
            'workspace_unchanged':list(workspace.iterdir())==[sentinel] and sha(sentinel)==before}
        cases.append(entry);report['measurements']['cases']=cases
        # A failed negative control cannot leave owned helpers behind. This
        # cooperative cleanup happens only after retaining the measured verdict.
        for socket in sockets:
            try: socket.sendall(b'x')
            except OSError: pass
    by_mode={row['mode']:row for row in cases}
    starts=[row for row in records if row['phase']=='start']
    holders=[row for row in records if row['phase']=='holder']
    report['checks']={
        'zero_exit_offered':by_mode['success']['exit_code']==0 and by_mode['success']['stdout'].strip()=='rust',
        'nonzero_exit_refused':by_mode['refusal']['exit_code']!=0 and 'no language server applies' in by_mode['refusal']['stderr'],
        'running_probe_offered':by_mode['pending']['exit_code']==0 and by_mode['pending']['stdout'].strip()=='rust',
        'running_probe_connection_retired':by_mode['pending']['all_connections_retired_before_fixture_cleanup'],
        'successful_parent_descendant_retired':by_mode['fork-success']['exit_code']==0 and by_mode['fork-success']['all_connections_retired_before_fixture_cleanup'],
        'refused_parent_descendant_retired':by_mode['fork-refusal']['exit_code']!=0 and by_mode['fork-refusal']['all_connections_retired_before_fixture_cleanup'],
        'each_probe_executed_once':len(starts)==5 and {row['mode'] for row in starts}==set(modes) and len(holders)==2 and {row['mode'] for row in holders}=={'fork-success','fork-refusal'},
        'stdin_closed':len(starts)==5 and all(row['stdin_eof'] for row in starts),
        'no_model_http':not http_requests,
        'workspace_unchanged':all(row['workspace_unchanged'] for row in cases),
        'binary_unchanged':sha(binary)==identity,
    }
    assert not errors,errors
    assert all(report['checks'].values()),report['checks']
    report['status']='passed';successful=True
finally:
    with lock: sockets=[socket for _,socket in connections]
    for socket in sockets:
        try: socket.sendall(b'x')
        except OSError: pass
    server.shutdown();server.server_close()
    report['http_requests']=http_requests
    with output.open('x',encoding='utf-8') as sink: json.dump(report,sink,indent=2)
    print(json.dumps(report),flush=True)
    # Retain failures for diagnosis. Only this exact fresh successful fixture
    # directory may be retired; no install/cache/user paths are involved.
    if successful:
        assert root.parent==parent and root.name.startswith('rook-lsp-detection-')
        shutil.rmtree(root)
