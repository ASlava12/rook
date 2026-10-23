"""Owned CLI/daemon regression for UTF-8 split across background pipe reads."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--expect-version')
parser.add_argument('--tmp-root',type=Path)
parser.add_argument('--stage',choices=['development','downloaded-release'],default='development')
parser.add_argument('--expect-corruption',action='store_true')
args=parser.parse_args();assert __debug__
output=args.output.resolve();assert output.parent.is_dir() and not output.exists()
root=Path(tempfile.mkdtemp(prefix='rook-background-utf8-',dir=args.tmp_root)).resolve()
expected=args.expect_version or tomllib.loads((Path(__file__).resolve().parents[3]/'Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version']
logs=root/'logs';logs.mkdir();empty=root/'empty-skills';empty.mkdir()
suffix='.exe' if os.name=='nt' else ''
bins={name:args.bin_dir.resolve()/(name+suffix) for name in ['rook','rookd']}
def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
helper=root/'emit unicode.py'
helper.write_text('''import pathlib,sys,time
root=pathlib.Path.cwd()
out=sys.stdout.buffer;err=sys.stderr.buffer
out.write(b'OUT '+bytes([0xe4]));out.flush()
err.write(b'ERR '+bytes([0xf0]));err.flush()
(root/'ready').write_text('ready')
deadline=time.monotonic()+20
while not (root/'release').exists():
    if time.monotonic()>deadline:raise SystemExit(70)
    time.sleep(.01)
for byte in bytes([0xbd,0xa0])+' Русский 🙂 OUT_END\\n'.encode():
    out.write(bytes([byte]));out.flush();time.sleep(.01)
time.sleep(.1)
for byte in bytes([0x9f,0x99,0x82])+' 你好 ERR_END\\n'.encode():
    err.write(bytes([byte]));err.flush();time.sleep(.01)
(root/'done').write_text('done')
''',encoding='utf-8')
cases={};errors=[];lock=threading.Lock();daemon=None;daemon_log=None
report={'stage':args.stage,'owned_root':str(root),'script_sha256':sha(__file__),'expected_version':expected,
    'expect_corruption':args.expect_corruption,'binaries':{},'cases':{},'checks':{}}
def tool_text(message):
    value=message['content']
    if isinstance(value,str):return value
    assert isinstance(value,list)
    return '\n'.join(p.get('text','') for p in value if isinstance(p,dict))
class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):super().setup();self.connection.settimeout(15)
    def log_message(self,*args):pass
    def do_POST(self):
        try:
            length=int(self.headers.get('Content-Length','0'));assert 0<length<=2*1024*1024
            name=self.path.split('/')[1];assert name in cases and self.path=='/'+name+'/v1/chat/completions'
            request=json.loads(self.rfile.read(length));case=cases[name]
            content=json.dumps(request['messages'])
            classifier='Classify whether an assistant' in content
            verifier='checking a claim somebody else made' in content
            with lock:
                case['requests']+=1;assert case['requests']<=8
                if classifier or verifier:case['auxiliary']+=1;index=-1
                else:index=case['main'];case['main']+=1;assert index<4
            text='{"action":"finish"}' if classifier else 'VERDICT: unproven' if verifier else 'OWNED_UTF8_DONE'
            call=None
            if index==0:
                argv=[sys.executable,str(helper)]
                command='"'+subprocess.list2cmdline(argv)+'"' if os.name=='nt' else shlex.join(argv)
                call=('run_command',{'command':command,'background':True})
            elif index==1:
                tool=next(m for m in reversed(request['messages']) if m['role']=='tool')
                match=re.search(r'\bjob[0-9]{3,}\b',tool_text(tool));assert match,tool_text(tool)[-2000:]
                case['job']=match.group(0);call=('job',{'id':case['job'],'wait_secs':1})
            elif index==2:
                tool=next(m for m in reversed(request['messages']) if m['role']=='tool')
                case['partial']=tool_text(tool)
                assert (case['workspace']/'ready').is_file(),'helper did not reach the split boundary'
                (case['workspace']/'release').write_bytes(b'release')
                call=('job',{'id':case['job'],'wait_secs':10})
            elif index==3:
                tool=next(m for m in reversed(request['messages']) if m['role']=='tool')
                case['final']=tool_text(tool)
            if call:
                delta={'role':'assistant','tool_calls':[{'index':0,'id':name+'-'+str(index),'type':'function',
                    'function':{'name':call[0],'arguments':json.dumps(call[1])}}]};ending='tool_calls'
            else:delta={'role':'assistant','content':text};ending='stop'
            if request.get('stream'):
                value={'model':'owned-utf8','choices':[{'index':0,'delta':delta,'finish_reason':ending}],
                    'usage':{'prompt_tokens':1,'completion_tokens':1}}
                data=('data: '+json.dumps(value)+'\n\ndata: [DONE]\n\n').encode();kind='text/event-stream'
            else:
                data=json.dumps({'model':'owned-utf8','choices':[{'message':delta,'finish_reason':ending}],
                    'usage':{'prompt_tokens':1,'completion_tokens':1}}).encode();kind='application/json'
            self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(data)))
            self.send_header('Connection','close');self.end_headers();self.wfile.write(data)
        except BaseException as error:
            with lock:errors.append(repr(error)[:3000])
            self.close_connection=True
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model);server.daemon_threads=True
threading.Thread(target=server.serve_forever,daemon=True).start()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
try:
    version_home=root/'version-home';version_home.mkdir()
    version_env=dict(os.environ,ROOK_HOME=str(version_home),ROOK_CONFIG_DIR=str(version_home))
    for name,path in bins.items():
        version=subprocess.check_output([str(path),'--version'],env=version_env,timeout=20).decode().strip();assert version==name+' '+expected
        report['binaries'][name]={'path':str(path),'version':version,'sha256':sha(path)}
    for name in ['cli','daemon']:
        print(json.dumps({'starting':name}),flush=True)
        case_root=root/name;home=case_root/'home';workspace=case_root/'workspace'
        home.mkdir(parents=True);workspace.mkdir();(workspace/'sentinel').write_bytes(b'unchanged')
        cases[name]={'workspace':workspace,'requests':0,'auxiliary':0,'main':0}
        env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(empty),ROOK_LOG='error',PYTHONIOENCODING='utf-8')
        (home/'config.toml').write_text(f'''[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=6
[memory]
enabled=false
[sandbox]
stance='autonomous'
isolate='off'
max_output_bytes=4096
[models.fixture]
api='openai'
model='owned-utf8'
context_window=131072
url='http://127.0.0.1:{server.server_port}/{name}/v1'
''',encoding='utf-8')
        if name=='daemon':
            daemon_log=(logs/'daemon.log').open('xb')
            daemon=subprocess.Popen([str(bins['rookd']),'--workspace',str(workspace),'--port','0'],env=env,cwd=workspace,
                stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=daemon_log,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            deadline=time.monotonic()+30
            while True:
                assert daemon.poll() is None and time.monotonic()<deadline,'owned daemon startup failed'
                try:
                    base=(home/'rookd.addr').read_text(encoding='utf-8').strip()
                    with opener.open(base+'/api/health',timeout=2) as response:assert response.status==200
                    break
                except (OSError,ValueError):time.sleep(.05)
        out=logs/(name+'.out');err=logs/(name+'.err')
        with out.open('xb') as stdout,err.open('xb') as stderr:
            result=subprocess.run([str(bins['rook']),'--workspace',str(workspace),'run','OWNED_UTF8_'+name.upper()],
                env=env,cwd=workspace,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,timeout=60,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        assert out.stat().st_size<=4*1024*1024 and err.stat().st_size<=128*1024
        with lock:
            case=cases[name];partial=case.get('partial','');final=case.get('final','')
            row={k:v for k,v in case.items() if k!='workspace'}
        checks={
            'client_completed':result.returncode==0 and b'OWNED_UTF8_DONE' in out.read_bytes(),
            'helper_completed':(workspace/'ready').exists() and (workspace/'release').exists() and (workspace/'done').exists(),
            'partial_was_running':'running for' in partial and 'OUT ' in partial and 'ERR ' in partial,
            'partial_clean':bool(partial) and '\ufffd' not in partial,
            'final_multilingual_exact':'你 Русский 🙂 OUT_END' in final and '🙂 你好 ERR_END' in final,
            'final_clean':bool(final) and '\ufffd' not in final,
            'job_exit_confirmed':'exit 0' in final,
            'bounded_request_count':case['main']==4 and case['requests']<=6,
            'workspace_sentinel_unchanged':(workspace/'sentinel').read_bytes()==b'unchanged',
            'execution_route_confirmed':(daemon.poll() is None and ('using the running rookd at '+base).encode() in err.read_bytes()) if name=='daemon' else b'using the running rookd at ' not in err.read_bytes(),
        }
        row.update(checks=checks,client_exit=result.returncode,stdout=str(out),stderr=str(err));report['cases'][name]=row
        report['checks'].update({name+'_'+key:value for key,value in checks.items()})
        print(json.dumps({'case':name,'checks':checks}),flush=True)
        if daemon is not None:
            daemon.terminate();daemon.wait(timeout=10);daemon_log.close();daemon=None;daemon_log=None
    report['server_errors']=errors
    report['checks']['no_model_protocol_errors']=not errors
    report['checks']['binary_pair_unchanged']=all(sha(bins[name])==row['sha256'] for name,row in report['binaries'].items())
    corrupt={'partial_clean','final_multilingual_exact','final_clean'}
    if args.expect_corruption:
        assert all(not row['checks'][key] for row in report['cases'].values() for key in corrupt),report['cases']
        assert all(value for key,value in report['checks'].items() if not any(key.endswith('_'+field) for field in corrupt))
        report['status']='expected_corruption_reproduced'
    else:
        assert all(v is True for v in report['checks'].values()),report['checks']
        report['status']='passed'
finally:
    if daemon is not None:
        if daemon.poll() is None:daemon.kill()
        daemon.wait(timeout=10)
        if daemon_log is not None:daemon_log.close()
    server.shutdown();server.server_close()
    output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'status':report['status'],'checks':len(report['checks']),'report':str(output)}),flush=True)
