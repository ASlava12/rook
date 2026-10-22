"""Actual doctor against owned inherited pipes and bounded/noisy version banners."""
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
    parser.error('Unoptimized Python, actual CLI, Rust compiler and fresh output path are required')
# Rustup selects its proxy by executable name. Keep rustc rather than resolving
# the hard/symbolic link to rustup.exe before changing the child's working dir.
compiler=os.path.abspath(compiler)
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
directory=Path(tempfile.mkdtemp(prefix='rook-version-capture-',dir=parent)).resolve()
assert directory.parent==parent and directory.name.startswith('rook-version-capture-')
inventory={'rustc','python3','java','git','cargo','npm'}
lock=threading.Lock()
holder_closed=threading.Event()
state={'starts':{},'events':[],'errors':[],'http_requests':0,'holder':None,'parent_exiting':None}

def sha(path):
    with path.open('rb') as source: return hashlib.file_digest(source,'sha256').hexdigest()

class Events(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(35)
        try:
            line=self.rfile.readline(257)
            assert len(line)<=256 and line.endswith(b'\n')
            text=line.decode('ascii').strip()
            if text.startswith(('GET ','POST ','PUT ','PATCH ','DELETE ')):
                with lock: state['http_requests']+=1
                raise AssertionError('doctor contacted the model endpoint')
            phase,name,pid=text.split()
            assert name in inventory and phase in ['start','finished','holder','parent-exiting']
            assert pid.isdigit() and len(pid)<=10
            record={'phase':phase,'tool':name,'pid':int(pid),'monotonic_ns':time.monotonic_ns()}
            with lock:
                assert len(state['events'])<64
                if phase=='start':
                    assert name not in state['starts']
                    state['starts'][name]=int(pid)
                if phase in ['holder','parent-exiting']:
                    assert name=='rustc'
                    key='holder' if phase=='holder' else 'parent_exiting'
                    assert state[key] is None
                    state[key]=record
                state['events'].append(record)
            self.wfile.write(b'ok\n');self.wfile.flush()
            if phase=='holder':
                try: assert self.rfile.read(1)==b''
                except ConnectionResetError: pass
                with lock:
                    state['holder_closed_ns']=time.monotonic_ns()
                holder_closed.set()
        except Exception as error:
            with lock:
                if len(state['errors'])<8: state['errors'].append(repr(error)[:500])

class Server(socketserver.ThreadingTCPServer):
    daemon_threads=True
    request_queue_size=32

server=Server(('127.0.0.1',0),Events)
threading.Thread(target=server.serve_forever,daemon=True).start()

def invoke(command,env,cwd,timeout):
    child=subprocess.Popen(command,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
        env=env,cwd=cwd,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    captures={'stdout':bytearray(),'stderr':bytearray()}
    overflow=threading.Event()
    def drain(name,stream):
        while chunk:=stream.read1(4096):
            room=65536-len(captures[name]);captures[name].extend(chunk[:room])
            if len(chunk)>room:
                overflow.set()
                if child.poll() is None: child.kill()
    readers=[threading.Thread(target=drain,args=(name,stream),daemon=True)
        for name,stream in [('stdout',child.stdout),('stderr',child.stderr)]]
    for reader in readers: reader.start()
    try: code=child.wait(timeout=timeout)
    finally:
        if child.poll() is None: child.kill();child.wait(timeout=10)
        for reader in readers: reader.join(timeout=10)
        assert not any(reader.is_alive() for reader in readers),'owned capture reader did not retire'
        child.stdout.close();child.stderr.close()
    assert code==0 and not overflow.is_set(),(code,bytes(captures['stdout'])[-2048:],bytes(captures['stderr'])[-2048:])
    return bytes(captures['stdout'])

identity=sha(binary)
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'measurements':state}
successful=False
try:
    for name in ['home','workspace','tools','empty']: (directory/name).mkdir()
    workspace=directory/'workspace'
    sentinel=workspace/'sentinel';sentinel.write_bytes(b'owned unchanged')
    (directory/'home/config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
[models.fixture]
api='openai'
model='owned-version-capture'
context_window=32768
url='http://127.0.0.1:{server.server_address[1]}/v1'
''',encoding='utf-8')
    helper=directory/('owned-version'+suffix)
    source=Path(__file__).with_name('helper.rs').resolve()
    invoke([compiler,str(source),'--edition','2024','-C','opt-level=s','-C','debuginfo=0',
        '-C','strip=symbols','-o',str(helper)],os.environ,directory,90)
    for name in inventory:
        path=directory/'tools'/(name+suffix)
        shutil.copy2(helper,path);path.chmod(path.stat().st_mode|0o111)
    env=dict(os.environ,ROOK_HOME=str(directory/'home'),ROOK_CONFIG_DIR=str(directory/'home'),
        ROOK_BUILTIN_SKILLS=str(directory/'empty'),PATH=str(directory/'tools'),ROOK_LOG='error',
        ROOK_OWNED_CAPTURE_ADDR='127.0.0.1:'+str(server.server_address[1]))
    started=time.monotonic()
    actual=json.loads(invoke([str(binary),'--workspace',str(workspace),'--json','doctor'],env,workspace,90))
    finished=time.monotonic_ns()
    report.update(status='completed',seconds=time.monotonic()-started,environment=actual,
        helper_sha256=sha(helper),helper_source_sha256=sha(source))
    retired=holder_closed.wait(timeout=3)
    with lock:
        holder=state['holder'];parent_exit=state['parent_exiting']
        held_seconds=None if holder is None else (finished-holder['monotonic_ns'])/1e9
        report['holder_to_doctor_seconds']=held_seconds
        report['checks']={
            'expected_language_versions':actual['languages']=={'python':'99.0.1','java':'99.0.1'},
            'oversized_tool_versions_rejected':actual['tools']=={},
            'each_owned_probe_runs_once':set(state['starts'])==inventory and len(state['starts'])==6 and not state['errors'],
            'inherited_pipe_deadline_preserved':holder is not None and parent_exit is not None
                and parent_exit['pid']==state['starts'].get('rustc') and 0<=held_seconds<20,
            'owned_holder_connection_retired':retired,
            'no_model_http_requests':state['http_requests']==0,
            'workspace_unchanged':sentinel.read_bytes()==b'owned unchanged' and set(workspace.iterdir())=={sentinel},
            'binary_unchanged':sha(binary)==identity,
        }
    assert len(report['checks'])==8 and all(report['checks'].values()),report['checks']
    successful=True
except BaseException as error:
    report['failure']=repr(error)[:1000]
    raise
finally:
    server.shutdown();server.server_close()
    if successful:
        assert directory.resolve().parent==parent and directory.name.startswith('rook-version-capture-')
        shutil.rmtree(directory)
    else: report['retained_owned_directory']=str(directory)
    with output.open('x',encoding='utf-8') as sink: json.dump(report,sink,indent=2)
    print(json.dumps(report),flush=True)
