"""Owned daemon terminal-evidence check; no live model or user state."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
if not __debug__: parser.error('Run without Python optimization.')
if args.output.exists() or not args.output.parent.is_dir(): parser.error('Choose a fresh output file in an existing directory.')
binary=args.bin_dir.resolve()/('rookd.exe' if os.name=='nt' else 'rookd')
assert binary.is_file()
scripts=Path(__file__).resolve().parent
labels=['NATIVE_TERMINAL_TOOL','NATIVE_TERMINAL_TEXT','NATIVE_TERMINAL_FINAL']
counts=dict.fromkeys(labels,0)
errors=[]
class Model(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)
    def log_message(self,*unused): pass
    def send(self,value,stream=False,finish=None):
        if stream:
            data=('data: '+json.dumps(value)+'\n\n'+('data: [DONE]\n\n' if finish else '')).encode()
        else: data=json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Type','text/event-stream' if stream else 'application/json')
        self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_GET(self): self.send({'data':[{'id':'terminal'}]})
    def do_POST(self):
        try:
            size=int(self.headers.get('content-length',0));assert 0<size<=2*1024*1024
            request=json.loads(self.rfile.read(size))
            text='\n'.join(str(m.get('content','')) for m in request['messages'] if m['role']=='user')
            label=next(label for label in labels if label in text)
            counts[label]+=1;assert counts[label]<=3
            if request.get('stream'):
                first=counts[label]==1
                final=label.endswith('FINAL')
                delta={'content':'' if final and first else 'Partial terminal fixture.' if first or final else 'Controlled answer.'}
                if first and (label.endswith('TOOL') or final):
                    delta['tool_calls']=[{'index':0,'id':'unconfirmed','type':'function','function':{
                        'name':'write_file','arguments':json.dumps({'path':'confirmed-effect.txt' if final else 'must-not-exist.txt','content':'controlled effect'})}}]
                choice={'index':0,'delta':delta}
                confirmed=(final and first) or (not final and not first)
                if confirmed: choice['finish_reason']='tool_calls' if final and first else 'stop'
                self.send({'model':'terminal','choices':[choice],'usage':{
                    'prompt_tokens':11 if first else 1,'completion_tokens':7 if first else 1}},True,confirmed)
            else:
                self.send({'model':'terminal','choices':[{'message':{'role':'assistant','content':'{"action":"finish"}'},
                            'finish_reason':'stop'}],'usage':{'prompt_tokens':1,'completion_tokens':1}})
        except Exception as error:
            if len(errors)<12:errors.append(repr(error)[:1000])
            self.send({'error':{'message':'owned fixture error'}})
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model)
threading.Thread(target=server.serve_forever,daemon=True).start()
daemon=None;clients=[]
temporary=tempfile.TemporaryDirectory(prefix='rook-terminal-native-')
root=Path(temporary.name)
try:
    home=root/'home';home.mkdir();workspace=root/'workspace';workspace.mkdir();empty=root/'empty';empty.mkdir()
    (workspace/'final.txt').write_text('original artifact',encoding='utf-8')
    (home/'config.toml').write_text(f'''[agent]
model='terminal'
install_servers=false
plan_first=false
one_script=false
max_steps=1
[sandbox]
stance='free'
[server]
port=0
[models.terminal]
model='terminal'
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
    env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(empty),ROOK_LOG='error')
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def get(url):
        with opener.open(url,timeout=10) as response:data=response.read(512*1024+1)
        assert len(data)<=512*1024
        return json.loads(data)
    def until(check,label):
        deadline=time.monotonic()+35
        while time.monotonic()<deadline:
            assert not errors,errors
            value=check()
            if value:return value
            time.sleep(.025)
        raise AssertionError(label)
    daemon=subprocess.Popen([str(binary),'--workspace',str(workspace),'--port','0'],env=env,cwd=workspace,
                            stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    def ready():
        assert daemon.poll() is None
        try:
            value=(home/'rookd.addr').read_text().strip();get(value+'/api/health');return value
        except (OSError,ValueError):return None
    base=until(ready,'daemon startup')
    statuses={};transcripts={}
    for label in labels:
        output=root/(label+'.json')
        client=subprocess.Popen(['node',str(scripts/'client.mjs'),base,label,str(output)],env=env,
                                stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        clients.append(client)
        until(lambda:client.poll() is not None,'chat terminal event')
        status=json.loads(output.read_text());assert client.returncode==0 and not status['fixture_error'],status
        statuses[label]=status
        transcripts[label]=get(base+'/api/sessions/'+status['session']+'/transcript?limit=100&max_body=4096')['items']
    def failed(label):
        status=statuses[label]
        return bool(status['failed'] and 'without confirmed completion' in status['failed'] and status['stopped']!='end_turn')
    def accounting(label):
        entries=transcripts[label]
        routes=[json.loads(e['body']) for e in entries if e['label']=='rook:model-route:v1']
        attempts=[json.loads(e['body']) for e in entries if e['label']=='rook:model-attempt:v1']
        return (sum(e['tokens_in'] for e in entries)==11 and sum(e['tokens_out'] for e in entries)==7
                and len(routes)==1 and routes[0]['complete'] is False and routes[0]['usage_reported'] is True
                and attempts[-1]['state']=='incomplete' and attempts[-1]['completion_confirmed'] is False)
    tool,text,final=labels
    checks={'tool_has_no_effects':not (workspace/'must-not-exist.txt').exists() and statuses[tool]['tool_done']==0
            and not any(e['label']=='write_file' for e in transcripts[tool]),
            'tool_stops_failed':failed(tool),'tool_one_request':counts[tool]==1,
            'tool_accounting_incomplete':accounting(tool),
            'text_stops_failed':failed(text),'text_one_request':counts[text]==1,
            'partial_text_retained':any(e['body']=='Partial terminal fixture.' for e in transcripts[text]),
            'text_accounting_incomplete':accounting(text)}
    entries=transcripts[final]
    auxiliary=[json.loads(e['body']) for e in entries if e['label']=='rook:model-aux:v1']
    attempts=[json.loads(e['body']) for e in entries if e['label']=='rook:model-attempt:v1']
    checks.update({
        'final_prior_effect_retained':(workspace/'confirmed-effect.txt').is_file() and statuses[final]['tool_done']==1,
        'final_output_preserved':(workspace/'final.txt').read_text(encoding='utf-8')=='original artifact',
        'final_stops_failed':failed(final),
        'final_requests_and_accounting':counts[final]==2 and sum(e['tokens_in'] for e in entries)==12
            and sum(e['tokens_out'] for e in entries)==8 and len(auxiliary)==1
            and auxiliary[0]['purpose']=='final_answer' and auxiliary[0]['receipt']['complete'] is False
            and attempts[-1]['state']=='incomplete'
            and attempts[-1]['completion_confirmed'] is False,
    })
    with binary.open('rb') as executable:digest=hashlib.file_digest(executable,'sha256').hexdigest()
    report={'checks':checks,'binary':str(binary),'binary_sha256':digest,'requests':counts,'statuses':statuses,
            'scope':'Owned daemon ordinary chats with scripted EOF, real fixture file and stored accounting; not live-model quality.'}
    with args.output.open('x',encoding='utf-8') as output:
        json.dump(report,output,indent=2)
    print(json.dumps(report),flush=True)
    assert all(checks.values()),report
finally:
    for client in clients:
        if client.poll() is None:
            client.terminate()
            try:client.wait(timeout=5)
            except subprocess.TimeoutExpired:client.kill();client.wait(timeout=5)
    if daemon is not None and daemon.poll() is None:
        daemon.terminate()
        try:daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:daemon.kill();daemon.wait(timeout=5)
    server.shutdown();server.server_close();temporary.cleanup()
