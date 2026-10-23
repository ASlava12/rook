"""Check balance refusal and normal throttling through the actual CLI transports."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import tomllib

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
parser.add_argument('--expect-version')
parser.add_argument('--stage',choices=['development','downloaded-release'],default='development')
args=parser.parse_args();assert __debug__
output=args.output.resolve();assert output.parent.is_dir() and not output.exists()
root=Path(tempfile.mkdtemp(prefix='rook-zai-balance-',dir=args.tmp_root)).resolve()
binary=args.bin_dir.resolve()/('rook.exe' if os.name=='nt' else 'rook')
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()
version=args.expect_version or tomllib.loads((Path(__file__).resolve().parents[3]/'Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version']
assert subprocess.check_output([str(binary),'--version'],timeout=10).decode().strip()=='rook '+version
report={'status':'running','stage':args.stage,'owned_root':str(root),'script_sha256':sha(__file__),
        'binary':str(binary),'binary_sha256':sha(binary),'version':version,'cases':{},'checks':{},'server_errors':[]}
empty=root/'empty-skills';empty.mkdir();retired=0
try:
    for api in ['openai','anthropic']:
        for code in ['1113','1302']:
            case=root/(api+'_'+code);case.mkdir();home=case/'home';home.mkdir();workspace=case/'workspace';workspace.mkdir()
            requests=[]
            class Handler(http.server.BaseHTTPRequestHandler):
                def log_message(self,*unused):pass
                def setup(self):super().setup();self.connection.settimeout(10)
                def do_GET(self):
                    data=b'{"data":[]}'
                    self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
                def do_POST(self):
                    try:
                        length=int(self.headers.get('Content-Length','0'));assert 0<length<=2*1024*1024
                        request=json.loads(self.rfile.read(length));assert len(requests)<8
                        requests.append({'path':self.path,'model':request.get('model')})
                        message='[1113][Insufficient balance or no resource package. Please recharge.]' if code=='1113' else 'Rate limit reached for requests'
                        data=json.dumps({'type':'error','error':{'type':'rate_limit_error','code':code,'message':message}}).encode()
                        self.send_response(429);self.send_header('Content-Type','application/json')
                        self.send_header('Content-Length',str(len(data)));self.send_header('Connection','close')
                        if code=='1113':self.send_header('Retry-After','120')
                        self.end_headers();self.wfile.write(data)
                    except BaseException as error:
                        if len(report['server_errors'])<16:report['server_errors'].append(repr(error)[:512])
                        self.close_connection=True
            server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
            thread=threading.Thread(target=server.serve_forever,kwargs={'poll_interval':.05},daemon=True);thread.start()
            try:
                base='http://127.0.0.1:'+str(server.server_port)+('/v1' if api=='openai' else '')
                config='skill_sources=[]\n[agent]\nmodel="fixture"\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=2\nmax_turn_secs=45\n[memory]\nenabled=false\n[models.fixture]\napi='+json.dumps(api)+'\nmodel="owned-model"\ncontext_window=131072\nurl='+json.dumps(base)+'\n'
                (home/'config.toml').write_text(config,encoding='utf-8')
                env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(empty),ROOK_LOG='error',RUST_BACKTRACE='0')
                out=case/'cli.stdout';err=case/'cli.stderr'
                with out.open('xb') as stdout,err.open('xb') as stderr:
                    result=subprocess.run([str(binary),'-C',str(workspace),'run','Reply only OK.'],cwd=workspace,env=env,
                                          stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,timeout=25,
                                          creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
                assert out.stat().st_size<=1024*1024 and err.stat().st_size<=1024*1024
                failure=err.read_text(encoding='utf-8');expected_path='/v1/chat/completions' if api=='openai' else '/v1/messages'
                checks={'original_refusal_returned':result.returncode==1 and 'provider returned 429' in failure and code in failure,
                        'physical_request_count':len(requests)==(1 if code=='1113' else 4),
                        'correct_transport_and_model':bool(requests) and all(r=={'path':expected_path,'model':'owned-model'} for r in requests),
                        'workspace_unchanged':not any(workspace.iterdir())}
                assert all(value is True for value in checks.values()),checks
                key=api+'_'+code
                report['cases'][key]={'checks':checks,'requests':requests,'cli_exit':result.returncode,'stderr':failure}
                report['checks'].update({key+'_'+k:v for k,v in checks.items()})
                print(json.dumps({'case':key,'requests':len(requests),'checks':checks}),flush=True)
            finally:
                server.shutdown();server.server_close();thread.join(timeout=2);assert not thread.is_alive();retired+=1
    report['checks'].update(binary_unchanged=sha(binary)==report['binary_sha256'],no_fixture_errors=not report['server_errors'],four_servers_retired=retired==4)
    assert all(value is True for value in report['checks'].values())
    report['status']='passed'
finally:
    output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'status':report['status'],'checks':len(report['checks']),'output':str(output)}),flush=True)
