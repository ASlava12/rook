"""Actual doctor JSON against sixteen owned tools; no user store or model."""
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
if not __debug__ or not binary.is_file() or output.exists() or not output.parent.is_dir() or not compiler:
    parser.error('Unoptimized Python, actual CLI, Rust compiler and fresh report path are required.')
temporary_root=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
directory=Path(tempfile.mkdtemp(prefix='rook-env-probes-',dir=temporary_root)).resolve()
assert directory.parent==temporary_root and directory.name.startswith('rook-env-probes-')
languages={'rustc':'rust','python3':'python','node':'node','go':'go','java':'java','ruby':'ruby','php':'php','dotnet':'dotnet'}
tools=['git','cargo','npm','uv','docker','kubectl','rg','gh']
inventory=set(languages)|set(tools)
lock=threading.Lock()
state={'starts':{},'ends':{},'active':{},'peak':0,'events':[],'errors':[],'http_requests':0}

def sha(path):
    with path.open('rb') as source: return hashlib.file_digest(source,'sha256').hexdigest()

class Events(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        try:
            line=self.rfile.readline(257)
            assert len(line)<=256 and line.endswith(b'\n')
            text=line.decode('ascii').strip()
            if text.startswith(('GET ','POST ','PUT ','PATCH ','DELETE ')):
                with lock: state['http_requests']+=1
                raise AssertionError('doctor JSON contacted the model endpoint')
            phase,name,pid=text.split()
            assert phase in ('start','end') and name in inventory and pid.isdigit() and len(pid)<=10
            pid=int(pid)
            with lock:
                assert len(state['events'])<64
                if phase=='start':
                    assert name not in state['starts']
                    state['starts'][name]=pid
                    state['active'][name]=pid
                    state['peak']=max(state['peak'],len(state['active']))
                else:
                    assert state['active'].get(name)==pid and name not in state['ends']
                    state['ends'][name]=pid
                    state['active'].pop(name)
                state['events'].append({'phase':phase,'tool':name,'pid':pid,'monotonic_ns':time.monotonic_ns()})
            self.wfile.write(b'ok\n');self.wfile.flush()
        except Exception as error:
            with lock:
                if len(state['errors'])<8: state['errors'].append(repr(error)[:500])

class Server(socketserver.ThreadingTCPServer):
    daemon_threads=True
    request_queue_size=32

server=Server(('127.0.0.1',0),Events)
threading.Thread(target=server.serve_forever,daemon=True).start()

def invoke(command,env,cwd,timeout):
    process=subprocess.Popen(command,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
        env=env,cwd=cwd,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    captures={'stdout':bytearray(),'stderr':bytearray()}
    overflow=threading.Event()
    def drain(stream,name):
        while chunk:=stream.read1(4096):
            room=65536-len(captures[name]);captures[name].extend(chunk[:room])
            if len(chunk)>room:
                overflow.set()
                if process.poll() is None: process.kill()
    readers=[threading.Thread(target=drain,args=(stream,name),daemon=True)
        for stream,name in [(process.stdout,'stdout'),(process.stderr,'stderr')]]
    for reader in readers: reader.start()
    try: code=process.wait(timeout=timeout)
    finally:
        if process.poll() is None: process.kill();process.wait(timeout=10)
        for reader in readers: reader.join(timeout=10)
        assert not any(reader.is_alive() for reader in readers),'owned output reader did not retire'
        process.stdout.close();process.stderr.close()
    assert not overflow.is_set(),'owned output exceeded64KiB per stream'
    assert code==0,(code,bytes(captures['stderr']).decode('utf-8',errors='replace')[-4096:])
    return {name:bytes(value) for name,value in captures.items()}

identity=sha(binary)
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'measurements':state}
successful=False
try:
    for name in ['home','workspace','tools','empty']: (directory/name).mkdir()
    home=directory/'home';workspace=directory/'workspace'
    sentinel=workspace/'sentinel';sentinel.write_bytes(b'owned unchanged')
    (home/'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
[models.fixture]
api='openai'
model='owned-environment'
context_window=32768
url='http://127.0.0.1:{server.server_address[1]}/v1'
''',encoding='utf-8')
    helper=directory/('owned-version'+suffix)
    invoke([compiler,str(Path(__file__).with_name('helper.rs').resolve()),'--edition','2024',
        '-C','opt-level=s','-C','debuginfo=0','-C','strip=symbols','-o',str(helper)],os.environ,directory,90)
    for name in inventory:
        path=directory/'tools'/(name+suffix)
        shutil.copy2(helper,path)
        path.chmod(path.stat().st_mode|0o111)
    env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(directory/'empty'),
        PATH=str(directory/'tools'),ROOK_LOG='error',ROOK_OWNED_PROBE_ADDR='127.0.0.1:'+str(server.server_address[1]))
    started=time.monotonic()
    result=invoke([str(binary),'--workspace',str(workspace),'--json','doctor'],env,workspace,75)
    report['seconds']=time.monotonic()-started
    actual=json.loads(result['stdout'])
    report.update(environment=actual,helper_sha256=sha(helper),status='completed')
    with lock:
        report['checks']={
            'exact_version_maps':actual['languages']=={name:'99.0.1' for name in languages.values()} and actual['tools']=={name:'99.0.1' for name in tools},
            'each_source_probe_runs_once':set(state['starts'])==inventory and len(state['starts'])==16,
            'all_owned_probes_finish':state['ends']==state['starts'] and not state['active'] and not state['errors'] and len(state['events'])==32,
            'parallel_execution_observed':state['peak']>1,
            'four_probe_cap_preserved':state['peak']<=4,
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
    # Failure evidence remains in its owned directory. Never remove mapped
    # helper files while a failed caller could still have a retiring child.
    if successful:
        assert directory.resolve().parent==temporary_root and directory.name.startswith('rook-env-probes-')
        shutil.rmtree(directory)
    else: report['retained_owned_directory']=str(directory)
    server.shutdown();server.server_close()
    with output.open('x',encoding='utf-8') as sink: json.dump(report,sink,indent=2)
print(json.dumps(report))
