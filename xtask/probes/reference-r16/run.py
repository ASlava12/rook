#!/usr/bin/env python3
"""Owned local daemon/model fixture; requires built rook/rookd and a Unix PTY.

No real model, credentials, installed daemon or user store is used.
"""
import argparse
import http.server
import json
import os
import pathlib
import select
import signal
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=pathlib.Path, required=True,
                    help='Directory containing the rook and rookd binaries')
parser.add_argument('--tmp-root', type=pathlib.Path,
                    help='Optional parent directory for owned temporary state')
args = parser.parse_args()
BINARY = args.bin_dir.resolve()
for name in ['rook', 'rookd']:
    if not (BINARY/name).is_file():
        parser.error(f'missing built binary: {BINARY/name}')
if not hasattr(os, 'openpty'):
    parser.error('this native terminal probe requires a Unix PTY')
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
seen = threading.Event(); release = threading.Event(); models = []
class LLM(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        data = json.dumps({'data':[{'id':'A'},{'id':'B'}]}).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_POST(self):
        self.connection.settimeout(30)
        size = int(self.headers.get('content-length', 0))
        assert 0 < size <= 2097152, 'fixture request limit reached'
        request = json.loads(self.rfile.read(size))
        model = request['model']; assert len(models)<64; models.append(model)
        text = '\n'.join(str(m.get('content','')) for m in request['messages'])
        classify = 'Classify whether' in str(request['messages'][0].get('content',''))
        if 'HELLO_BOUNDARY' in text and not classify and not seen.is_set():
            seen.set()
            if not release.wait(30): raise RuntimeError('owned model gate expired')
        answer = '{"action":"finish"}' if classify else 'Hello from model '+model
        chunk = {'id':'owned-response','model':model,'choices':[{'index':0,'delta':{'role':'assistant','content':answer},'finish_reason':None}]}
        end = {'id':'owned-response','model':model,'choices':[{'index':0,'delta':{},'finish_reason':'stop'}], 'usage':{'prompt_tokens':100,'completion_tokens':5,'total_tokens':105}}
        if request.get('stream'):
            data = ('data: '+json.dumps(chunk)+'\n\ndata: '+json.dumps(end)+'\n\ndata: [DONE]\n\n').encode(); content_type = 'text/event-stream'
        else:
            data = json.dumps({'id':'owned-response','model':model,'choices':[{'index':0,'message':{'role':'assistant','content':answer},'finish_reason':'stop'}],'usage':{'prompt_tokens':100,'completion_tokens':5,'total_tokens':105}}).encode(); content_type = 'application/json'
        self.send_response(200); self.send_header('Content-Type',content_type); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
server = http.server.ThreadingHTTPServer(('127.0.0.1',0), LLM)
threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix='rook-r16-frontend-',dir=args.tmp_root) as directory:
    root=pathlib.Path(directory); home=root/'home'; home.mkdir(); workspace=root/'workspace'; workspace.mkdir()
    env=dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_LOG='error', TMPDIR=str(root))
    (home/'config.toml').write_text(f'''[agent]\nmodel='A'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=4\n[memory]\nenabled=false\n[web]\nenabled=false\n[server]\nport=0\n[telemetry.runtime_diagnostics]\nenabled=true\ninterval_ms=50\nslow_ms=1\n[models.A]\nmodel='A'\napi='openai'\nurl='http://127.0.0.1:{server.server_port}/v1'\ncontext_window=32768\n[models.B]\nmodel='B'\napi='openai'\nurl='http://127.0.0.1:{server.server_port}/v1'\ncontext_window=32768\n[models.B.compaction]\ncompact_at=0.6\nreserve_output=2048\nkeep_recent_tokens=3000\n''')
    def cli(*args, input=None):
        p=subprocess.run([str(BINARY/'rook'),'--workspace',str(workspace),*args],env=env,cwd=workspace,input=input,text=True,capture_output=True,timeout=40)
        assert p.returncode == 0, (args,p.returncode,p.stdout[-2000:],p.stderr[-2000:])
        return p.stdout
    cli('chat', input='/quit\n')
    sid=json.loads(cli('--json','session','ls'))[0]['id']
    original=json.loads(cli('--json','session','model',sid,'A','--request-id','pick-A','--revision','0'))
    daemon=None; run=None
    def start():
        child=subprocess.Popen([str(BINARY/'rookd'),'--workspace',str(workspace),'--port','0'],env=env,cwd=workspace,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        deadline=time.monotonic()+30
        while time.monotonic()<deadline:
            path=home/'rookd.addr'
            if path.exists():
                base=path.read_text().strip()
                try:
                    with opener.open(base+'/api/health',timeout=1) as response: json.load(response)
                    return child,base
                except Exception: pass
            assert child.poll() is None, 'owned daemon exited during startup'
            time.sleep(.05)
        child.kill();child.wait();raise RuntimeError('owned daemon did not start')
    try:
        daemon,base=start()
        def api(path,body=None):
            data=None if body is None else json.dumps(body).encode()
            request=urllib.request.Request(base+path,data=data,headers={'Content-Type':'application/json'} if data else {})
            with opener.open(request,timeout=10) as response: return json.load(response)
        run=subprocess.Popen([str(BINARY/'rook'),'--workspace',str(workspace),'run','--session',sid,'Say HELLO_BOUNDARY'],env=env,cwd=workspace,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert seen.wait(20), 'owned LLM never received the first root request'
        before=time.monotonic()
        latest=api('/api/sessions/'+sid+'/model-selection',{'id':'pick-B','expected_revision':1,'model':'B'})
        busy_ms=round((time.monotonic()-before)*1000)
        assert busy_ms < 5000, 'busy model mutation waited for a held generation'
        assert api('/api/sessions/'+sid+'/model-selection',{'id':'pick-A','expected_revision':0,'model':'A'}) == original
        view=api('/api/sessions/'+sid+'/model-selection'); assert view['selected']=='B' and view['pending'],view
        assert models[-1]=='A',models
        release.set();out,err=run.communicate(timeout=30); assert run.returncode==0,(out,err)
        assert 'Hello from model A' in out,(out,err)
        out=cli('run','--session',sid,'Say hello again')
        assert 'Hello from model B' in out,out
        view=api('/api/sessions/'+sid+'/model-selection'); assert view['selected']=='B' and view['last_effective']=='B' and not view['pending'],view
        usage=api('/api/sessions/'+sid+'/context'); assert usage['usable']==32768-2048,usage
        share=api('/api/sessions/'+sid+'/export',{'profile':'share'}); assert share['events']>=4 and share['omitted'],share
        preview=json.loads(cli('session','export-html',sid,'--preview')); assert 'html' not in preview and len(preview['preview'])<=8
        assert api('/api/attention')['items']==[]
        report=api('/api/sessions/'+sid+'/diagnostics'); assert report['runtime'] and report['runtime']['aggregates'],report
        daemon.send_signal(signal.SIGTERM);daemon.wait(timeout=30);daemon=None
        daemon,base=start()
        assert api('/api/sessions/'+sid+'/model-selection',{'id':'pick-A','expected_revision':0,'model':'A'})==original
        assert api('/api/sessions/'+sid+'/model-selection')['selected']=='B'
        local=cli('session','export-html',sid,'--profile','share','--output',str(root/'share.html'))
        assert (root/'share.html').read_text()==api('/api/sessions/'+sid+'/export',{'profile':'share'})['html']
        def pty_run(json_mode=False):
            master, slave = os.openpty()
            args=[str(BINARY/'rook'),'--workspace',str(workspace)]
            if json_mode: args.append('--json')
            args.extend(['run','--session',sid,'Say a terminal hello'])
            child=subprocess.Popen(args,env=dict(env,TERM='xterm-256color',ROOK_PROGRAM_STATUS='on',ROOK_NOTIFY='off'),cwd=workspace,stdin=slave,stdout=slave,stderr=slave)
            os.close(slave); output=bytearray(); deadline=time.monotonic()+40
            try:
                while time.monotonic()<deadline:
                    if select.select([master],[],[],.1)[0]:
                        try: chunk=os.read(master,8192)
                        except OSError: break
                        if not chunk: break
                        output.extend(chunk)
                        assert len(output)<1048576
                assert child.wait(timeout=5)==0,bytes(output[-4096:])
            finally:
                os.close(master)
                if child.poll() is None: child.kill();child.wait()
            return bytes(output)
        tty=pty_run()
        for state in [b'working',b'done',b'clear']:
            assert b'\x1b]7501;state='+state+b':app=rook\x1b\\' in tty,tty
        assert b'\x1b]7501;' not in pty_run(True)
        print(json.dumps({'busy_selection_ms':busy_ms,'models':models,'selection':view,'share_events':share['events'],'omitted':share['omitted'],'runtime_metrics':len(report['runtime']['aggregates']),'exact_retry_after_restart':True,'api_cli_export_parity':True,'terminal_pty_lifecycle':True,'json_pty_no_osc':True}))
    finally:
        release.set()
        if run is not None and run.poll() is None:
            run.terminate()
            try: run.wait(timeout=10)
            except subprocess.TimeoutExpired: run.kill();run.wait()
        if daemon is not None:
            daemon.terminate()
            try:daemon.wait(timeout=20)
            except subprocess.TimeoutExpired:daemon.kill();daemon.wait()
server.shutdown();server.server_close()
