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
import shlex
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=pathlib.Path, required=True,
                    help='Directory containing the rook and rookd binaries')
parser.add_argument('--tmp-root', type=pathlib.Path,
                    help='Optional parent directory for owned temporary state')
parser.add_argument('--browser', type=pathlib.Path, help='Optional Chrome/Chromium executable for real UI checks')
parser.add_argument('--followup', action='store_true', help='Also verify the post-0.17 observer, tool-status and crash-loop contracts')
args = parser.parse_args()
BINARY = args.bin_dir.resolve()
for name in ['rook', 'rookd']:
    if not (BINARY/name).is_file():
        parser.error(f'missing built binary: {BINARY/name}')
if not hasattr(os, 'openpty'):
    parser.error('this native terminal probe requires a Unix PTY')
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
seen = threading.Event(); release = threading.Event(); models = []
crash_entered = threading.Condition(); crash_count = 0; crash_release = threading.Event()
class LLM(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        data = json.dumps({'data':[{'id':'A'},{'id':'B'}]}).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_POST(self):
        global crash_count
        self.connection.settimeout(30)
        size = int(self.headers.get('content-length', 0))
        assert 0 < size <= 2097152, 'fixture request limit reached'
        request = json.loads(self.rfile.read(size))
        model = request['model']; assert len(models)<64; models.append((model,request.get('reasoning_effort')))
        text = '\n'.join(str(m.get('content','')) for m in request['messages'])
        classify = 'Classify whether' in str(request['messages'][0].get('content',''))
        if args.followup and 'CHECK_CRASH_GUARD' in text and not classify:
            with crash_entered:
                crash_count += 1; crash_entered.notify_all()
            if not crash_release.wait(90): raise RuntimeError('owned crash gate expired')
        if 'HELLO_BOUNDARY' in text and not classify and not seen.is_set():
            seen.set()
            if not release.wait(30): raise RuntimeError('owned model gate expired')
        answer = '{"action":"finish"}' if classify else 'Hello from model '+model
        chunk = {'id':'owned-response','model':model,'choices':[{'index':0,'delta':{'role':'assistant','content':answer},'finish_reason':None}]}
        end = {'id':'owned-response','model':model,'choices':[{'index':0,'delta':{},'finish_reason':'stop'}], 'usage':{'prompt_tokens':100,'completion_tokens':5,'total_tokens':105}}
        last_user = next((str(m.get('content','')) for m in reversed(request['messages']) if m.get('role')=='user'), '')
        if args.followup and 'CHECK_TOOL_STATUS' in last_user and not classify and request['messages'][-1].get('role')!='tool':
            assert any(tool.get('function',{}).get('name')=='tool_status' for tool in request.get('tools',[])), 'tool_status missing from the actual wire request'
            chunk['choices'][0]['delta']={'role':'assistant','tool_calls':[{'index':0,'id':'status-probe','type':'function','function':{'name':'tool_status','arguments':'{"query":"recall"}'}}]}
            end['choices'][0]['finish_reason']='tool_calls'
        if request.get('stream'):
            data = ('data: '+json.dumps(chunk)+'\n\ndata: '+json.dumps(end)+'\n\ndata: [DONE]\n\n').encode(); content_type = 'text/event-stream'
        else:
            data = json.dumps({'id':'owned-response','model':model,'choices':[{'index':0,'message':{'role':'assistant','content':answer},'finish_reason':'stop'}],'usage':{'prompt_tokens':100,'completion_tokens':5,'total_tokens':105}}).encode(); content_type = 'application/json'
        try:
            self.send_response(200); self.send_header('Content-Type',content_type); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            if not (args.followup and 'CHECK_CRASH_GUARD' in text): raise
server = http.server.ThreadingHTTPServer(('127.0.0.1',0), LLM)
threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix='rook-r17-frontend-',dir=args.tmp_root) as directory:
    root=pathlib.Path(directory); home=root/'home'; home.mkdir(); workspace=root/'workspace'; workspace.mkdir()
    env=dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_LOG='error', TMPDIR=str(root))
    (home/'config.toml').write_text(f'''[agent]\nmodel='A'\ninstall_servers=false\nplan_first=false\none_script=false\nmax_steps=4\n[memory]\nenabled=false\n[web]\nenabled=false\n[server]\nport=0\n[telemetry.runtime_diagnostics]\nenabled=true\ninterval_ms=50\nslow_ms=1\n[models.A]\nmodel='A'\napi='openai'\nurl='http://127.0.0.1:{server.server_port}/v1'\ncontext_window=32768\n[models.A.reasoning]\ncontrol='openai-effort'\n[models.B]\nmodel='B'\napi='openai'\nurl='http://127.0.0.1:{server.server_port}/v1'\ncontext_window=32768\n[models.B.reasoning]\ncontrol='openai-effort'\n[models.B.compaction]\ncompact_at=0.6\nreserve_output=2048\nkeep_recent_tokens=3000\n''')
    def cli(*args, input=None):
        p=subprocess.run([str(BINARY/'rook'),'--workspace',str(workspace),*args],env=env,cwd=workspace,input=input,text=True,capture_output=True,timeout=40)
        assert p.returncode == 0, (args,p.returncode,p.stdout[-2000:],p.stderr[-2000:])
        return p.stdout
    if args.followup:
        callback=root/'observer.py'; receipts=root/'deletions.jsonl'
        callback.write_text('import json,sys\npayload=json.load(sys.stdin)\nwith open(sys.argv[1], "a") as output: output.write(json.dumps(payload)+"\\n")\n')
        with (home/'config.toml').open('a') as config:
            config.write('\n[work]\nrestart_limit=2\n[[hooks]]\nevent="session_delete"\ncommand='+json.dumps(shlex.join([sys.executable,str(callback),str(receipts)]))+'\n')
        cli('chat',input='/quit\n')
        deleted=json.loads(cli('--json','session','ls'))[0]['id']
        cli('session','rm',deleted)
        assert json.loads(receipts.read_text().splitlines()[0])['session']==deleted
    cli('chat', input='/quit\n')
    sid=json.loads(cli('--json','session','ls'))[0]['id']
    original=json.loads(cli('--json','session','model',sid,'A','--request-id','pick-A','--revision','0','--effort','low'))
    daemon=None; run=None
    def start(executable=None):
        child=subprocess.Popen([str(executable or BINARY/'rookd'),'--workspace',str(workspace),'--port','0'],env=env,cwd=workspace,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
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
        def api(path,body=None,method=None):
            data=None if body is None else json.dumps(body).encode()
            request=urllib.request.Request(base+path,data=data,headers={'Content-Type':'application/json'} if data else {},method=method)
            with opener.open(request,timeout=10) as response: return json.load(response)
        run=subprocess.Popen([str(BINARY/'rook'),'--workspace',str(workspace),'run','--session',sid,'Say HELLO_BOUNDARY'],env=env,cwd=workspace,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert seen.wait(20), 'owned LLM never received the first root request'
        background=api('/api/sessions/'+sid+'/background')
        selected=next(e for e in background['entries'] if e['session']==sid)
        assert selected['owner_live'] is True and selected['activity']['stage']=='model',background
        assert 'HELLO_BOUNDARY' not in json.dumps([background['entries'],background['jobs']]), 'activity/job metadata copied the active prompt'
        cli_background=json.loads(cli('--json','session','background',sid))
        assert cli_background['tree']['selected']['id']==sid and cli_background['entries'][0]['owner_live'] is True
        before=time.monotonic()
        latest=api('/api/sessions/'+sid+'/model-selection',{'id':'pick-B','expected_revision':1,'model':'B','effort':'high'})
        busy_ms=round((time.monotonic()-before)*1000)
        assert busy_ms < 5000, 'busy model mutation waited for a held generation'
        assert api('/api/sessions/'+sid+'/model-selection',{'id':'pick-A','expected_revision':0,'model':'A','effort':'low'}) == original
        view=api('/api/sessions/'+sid+'/model-selection'); assert view['selected']=='B' and view['pending'],view
        assert models[-1]==('A','low'),models
        assert view['selected_effort']=='high' and view['last_effective_effort']=='low',view
        release.set();out,err=run.communicate(timeout=30); assert run.returncode==0,(out,err)
        assert 'Hello from model A' in out,(out,err)
        out=cli('run','--session',sid,'Say hello again')
        assert 'Hello from model B' in out,out
        assert any(pair==('B','high') for pair in models),models
        answer=api('/api/sessions/'+sid+'/last-answer'); assert answer['text']=='Hello from model B',answer
        assert cli('session','last-answer',sid).strip()==answer['text']
        assert api('/api/sessions/'+sid+'/delegated-results')==[]
        view=api('/api/sessions/'+sid+'/model-selection'); assert view['selected']=='B' and view['last_effective']=='B' and not view['pending'],view
        usage=api('/api/sessions/'+sid+'/context'); assert usage['usable']==32768-2048,usage
        share=api('/api/sessions/'+sid+'/export',{'profile':'share'}); assert share['events']>=4 and share['omitted'],share
        preview=json.loads(cli('session','export-html',sid,'--preview')); assert 'html' not in preview and len(preview['preview'])<=8
        assert api('/api/attention')['items']==[]
        report=api('/api/sessions/'+sid+'/diagnostics'); assert report['runtime'] and report['runtime']['aggregates'],report
        daemon.send_signal(signal.SIGTERM);daemon.wait(timeout=30);daemon=None
        daemon,base=start()
        assert api('/api/sessions/'+sid+'/model-selection',{'id':'pick-A','expected_revision':0,'model':'A','effort':'low'})==original
        assert api('/api/sessions/'+sid+'/model-selection')['selected']=='B'
        if args.followup:
            job=api('/api/work',{'goal':'CHECK_CRASH_GUARD','workspace':str(workspace),'autonomous':False,'max_iterations':10,'max_tokens':100000,'max_seconds':300})
            for count in [1,2]:
                with crash_entered:
                    assert crash_entered.wait_for(lambda:crash_count>=count,timeout=30), 'owned worker did not reach the physical request'
                saved=api('/api/work/'+job['id'])
                assert saved['restart_guard']=={'unfinished_stage':True,'consecutive_interruptions':count-1,'limit':2},saved
                daemon.kill();daemon.wait(timeout=10);daemon=None
                daemon,base=start()
            until=time.monotonic()+30
            while time.monotonic()<until:
                saved=api('/api/work/'+job['id'])
                if saved['status']=='blocked':break
                time.sleep(.05)
            else:raise AssertionError('crash-loop guard did not park the restarted worker')
            assert crash_count==2 and saved['restart_guard']['consecutive_interruptions']==2 and saved['consecutive_failures']==0,saved
            assert 'restart limit 2' in saved['reason']
            shown=cli('task','show',job['id']); assert 'interrupted stage recovery: 2 / 2' in shown,shown
            api('/api/work/'+job['id']+'/control','cancel');crash_release.set()
        drain=api('/api/daemon/drain',{'action':'start','id':'probe-drain','timeout_secs':30,'restart':False})
        until=time.monotonic()+10
        while drain['phase']=='draining' and time.monotonic()<until:
            time.sleep(.05);drain=api('/api/daemon/drain')
        assert drain['phase']=='ready',drain
        try:
            api('/api/sessions/'+sid+'/model-selection',{'id':'blocked','expected_revision':2,'model':'A','effort':'max'})
            raise AssertionError('drain admitted a fresh model mutation')
        except urllib.error.HTTPError as error: assert error.code==503
        assert api('/api/health')['turns_running']==0
        assert api('/api/daemon/drain',{'action':'cancel','id':'probe-drain'})['phase']=='cancelled'
        api('/api/daemon/drain',{'action':'start','id':'probe-restart','timeout_secs':30,'restart':True})
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            path=home/'run/drain-restart.json'
            if path.exists():
                receipt=json.loads(path.read_text())
                if receipt['phase']=='failed': raise AssertionError(receipt)
                if receipt['phase']=='restarted': break
            time.sleep(.05)
        else: raise AssertionError('owned restart handoff did not finish')
        daemon.wait(timeout=10); daemon=None
        base=(home/'rookd.addr').read_text().strip()
        assert api('/api/daemon/drain')['handoff']['phase']=='restarted'
        assert api('/api/sessions/'+sid+'/model-selection')['selected_effort']=='high'
        if args.browser:
            api('/api/presets',{'action':'save','name':'r17-pair','preset':{'prompt':'R17_PRESET_DRAFT','model':'A','effort':'max'},'expected':None})
            profile=root/'chrome-profile';profile.mkdir()
            browser=subprocess.Popen([str(args.browser),'--headless=new','--no-first-run','--no-default-browser-check','--remote-debugging-port=0','--user-data-dir='+str(profile),'about:blank'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:
                probe=pathlib.Path(__file__).with_name('browser.mjs').resolve()
                p=subprocess.run(['node',str(probe),base,sid,str(profile)],capture_output=True,text=True,timeout=120)
                assert p.returncode==0,(p.stdout,p.stderr)
                print(p.stdout.strip())
            finally:
                browser.terminate()
                try:browser.wait(timeout=10)
                except subprocess.TimeoutExpired:browser.kill();browser.wait()
            current=api('/api/sessions/'+sid+'/model-selection')
            api('/api/sessions/'+sid+'/model-selection',{'id':'after-browser','expected_revision':current['revision'],'model':'B','effort':'high'})
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
        cli('daemon','stop')
        until=time.monotonic()+20
        while (home/'rookd.addr').exists() and time.monotonic()<until:time.sleep(.05)
        assert not (home/'rookd.addr').exists()
        faulty=root/'faulty-install';faulty.mkdir()
        os.link(BINARY/'rookd',faulty/'rookd')
        (faulty/'rook').write_text('#!/bin/sh\nexit 73\n');(faulty/'rook').chmod(0o700)
        daemon,base=start(faulty/'rookd')
        api('/api/daemon/drain',{'action':'start','id':'failed-handoff','timeout_secs':30,'restart':True})
        until=time.monotonic()+10
        while time.monotonic()<until:
            failed=api('/api/daemon/drain')
            if failed['phase']=='failed' and failed.get('handoff',{}).get('phase')=='failed':break
            time.sleep(.05)
        else:raise AssertionError('failed controller was not reported')
        assert api('/api/health')['turns_running']==0 and daemon.poll() is None
        assert failed['handoff']['phase']=='failed',failed
        assert api('/api/daemon/drain',{'action':'cancel','id':'failed-handoff'})['phase']=='cancelled'
        if args.followup:
            cli('run','--session',sid,'CHECK_TOOL_STATUS')
            transcript=json.loads(cli('--json','session','show',sid))
            status=next(json.loads(row['body']) for row in reversed(transcript) if row['kind']=='tool-result' and row['label']=='tool_status')
            assert status['items'][0]['status']=='unavailable' and 'memory is disabled' in status['items'][0]['reason'],status
            assert api('/api/sessions/'+sid,method='DELETE')['events']>0
            assert api('/api/sessions/'+sid,method='DELETE')['events']==0
            deleted_payloads=[json.loads(line) for line in receipts.read_text().splitlines()]
            assert [item['session'] for item in deleted_payloads]==[deleted,sid],deleted_payloads
            print(json.dumps({'offline_cli_delete_observer':True,'daemon_delete_observer_once':True,'wire_tool_status_dispatch':True,'physical_crash_recovery_limit':True,'restart_limit_cli_status':True}))
        print(json.dumps({'busy_selection_ms':busy_ms,'models':models,'selection':view,'share_events':share['events'],'omitted':share['omitted'],'runtime_metrics':len(report['runtime']['aggregates']),'exact_retry_after_restart':True,'api_cli_export_parity':True,'paired_effort':True,'copy_read_parity':True,'drain_cancel_and_restart':True,'failed_controller_keeps_old_daemon':True,'terminal_pty_lifecycle':True,'json_pty_no_osc':True,'background_busy_owner_and_cli':True}))
    finally:
        release.set()
        crash_release.set()
        if daemon is None and (home/'rookd.addr').exists():
            try:cli('daemon','stop','--force')
            except Exception:pass
            until=time.monotonic()+15
            while (home/'rookd.addr').exists() and time.monotonic()<until:time.sleep(.1)
        if run is not None and run.poll() is None:
            run.terminate()
            try: run.wait(timeout=10)
            except subprocess.TimeoutExpired: run.kill();run.wait()
        if daemon is not None:
            daemon.terminate()
            try:daemon.wait(timeout=20)
            except subprocess.TimeoutExpired:daemon.kill();daemon.wait()
server.shutdown();server.server_close()
