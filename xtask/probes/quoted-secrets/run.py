"""Exercise actual CLI/daemon transport, saved calls and live context with an artificial key."""
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
parser.add_argument('--tmp-root',type=Path)
parser.add_argument('--expect-version',required=True)
parser.add_argument('--source',required=True)
parser.add_argument('--stage',choices=['baseline','development','downloaded-release'],required=True)
args=parser.parse_args();assert __debug__
destination=args.output.resolve();assert destination.parent.is_dir() and not destination.exists()
root=Path(tempfile.mkdtemp(prefix='rook-quoted-secret-',dir=args.tmp_root)).resolve()
suffix='.exe' if os.name=='nt' else ''
bins={name:args.bin_dir.resolve()/(name+suffix) for name in ['rook','rookd']}
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()
binary_identity={name:{'path':str(path),'version':subprocess.check_output([str(path),'--version'],timeout=10).decode().strip(),'sha256':sha(path)} for name,path in bins.items()}
assert all(row['version']==name+' '+args.expect_version for name,row in binary_identity.items())
report={'status':'running','source':args.source,'stage':args.stage,'version':args.expect_version,'script_sha256':sha(__file__),
        'owned_root':str(root),'binaries':binary_identity,'cases':{},'checks':{},'server_errors':[]}
secret=r'owned-"quoted"\fixture-credential'
escaped=json.dumps(secret,ensure_ascii=False)[1:-1]
empty=root/'empty-skills';empty.mkdir();retired=0
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))

def has_value(value):
    if isinstance(value,str):return secret in value or escaped in value
    if isinstance(value,list):return any(has_value(item) for item in value)
    if isinstance(value,dict):return any(has_value(key) or has_value(item) for key,item in value.items())
    return False

try:
    for mode,api in [('local','openai'),('daemon','openai'),('local','anthropic'),('daemon','anthropic')]:
        name=mode+'_'+api
        case=root/name;case.mkdir()
        home=case/'home';workspace=case/'workspace';source=case/'empty-source'
        for path in [home,workspace,source]:path.mkdir()
        observations=[];later=[];main_count=0;aux_count=0
        class Model(http.server.BaseHTTPRequestHandler):
            def log_message(self,*unused):pass
            def setup(self):super().setup();self.connection.settimeout(10)
            def do_GET(self):
                data=b'{"data":[]}'
                self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
            def do_POST(self):
                global main_count,aux_count
                try:
                    length=int(self.headers.get('Content-Length','0'));assert 0<length<=2*1024*1024
                    request=json.loads(self.rfile.read(length));assert len(observations)<8
                    messages=request['messages'];text=json.dumps(request)
                    auxiliary='Classify whether an assistant' in text or 'checking a claim somebody else made' in text
                    header_ok=(self.headers.get('Authorization')=='Bearer '+secret) if api=='openai' else (self.headers.get('x-api-key')==secret)
                    observations.append({'path':self.path,'model':request.get('model'),'original_credential_header':header_ok,'phase':'auxiliary' if auxiliary else 'main'})
                    if auxiliary:
                        aux_count+=1
                        message={'role':'assistant','content':'{"action":"finish"}' if 'Classify whether an assistant' in text else 'VERDICT: unproven'};finish='stop'
                    else:
                        main_count+=1
                        if main_count==1:
                            call={'id':'owned_query','type':'function','function':{'name':'find_skill','arguments':json.dumps({'query':secret})}}
                            message={'role':'assistant','content':'','tool_calls':[call]};finish='tool_calls'
                        else:
                            later.append(messages)
                            message={'role':'assistant','content':'OWNED_QUOTED_KEY_OK'};finish='stop'
                    if api=='anthropic':
                        tool=message.get('tool_calls',[None])[0]
                        block=({'type':'tool_use','id':tool['id'],'name':tool['function']['name'],'input':json.loads(tool['function']['arguments'])}
                               if tool else {'type':'text','text':message['content']})
                        reason='tool_use' if tool else 'end_turn'
                        if request.get('stream'):
                            initial=dict(block,input={}) if tool else dict(block,text='')
                            delta={'type':'input_json_delta','partial_json':json.dumps(block['input'])} if tool else {'type':'text_delta','text':block['text']}
                            events=[('message_start',{'type':'message_start','message':{'id':'owned_msg','type':'message','role':'assistant','model':'owned-quoted-model','content':[],'usage':{'input_tokens':1,'output_tokens':0}}}),
                                    ('content_block_start',{'type':'content_block_start','index':0,'content_block':initial}),
                                    ('content_block_delta',{'type':'content_block_delta','index':0,'delta':delta}),
                                    ('content_block_stop',{'type':'content_block_stop','index':0}),
                                    ('message_delta',{'type':'message_delta','delta':{'stop_reason':reason},'usage':{'output_tokens':1}}),
                                    ('message_stop',{'type':'message_stop'})]
                            data=''.join('event: '+event+'\ndata: '+json.dumps(value)+'\n\n' for event,value in events).encode();kind='text/event-stream'
                        else:
                            data=json.dumps({'id':'owned_msg','type':'message','role':'assistant','model':'owned-quoted-model','content':[block],'stop_reason':reason,'usage':{'input_tokens':1,'output_tokens':1}}).encode();kind='application/json'
                    elif request.get('stream'):
                        delta=dict(message)
                        if 'tool_calls' in delta:delta['tool_calls']=[dict(call,index=index) for index,call in enumerate(delta['tool_calls'])]
                        value={'model':'owned-quoted-model','choices':[{'index':0,'delta':delta,'finish_reason':finish}],
                               'usage':{'prompt_tokens':1,'completion_tokens':1}}
                        data=('data: '+json.dumps(value)+'\n\ndata: [DONE]\n\n').encode();kind='text/event-stream'
                    else:
                        data=json.dumps({'model':'owned-quoted-model','choices':[{'message':message,'finish_reason':finish}],
                                         'usage':{'prompt_tokens':1,'completion_tokens':1}}).encode();kind='application/json'
                    self.send_response(200);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(data)))
                    self.send_header('Connection','close');self.end_headers();self.wfile.write(data)
                except BaseException as error:
                    if len(report['server_errors'])<16:report['server_errors'].append(repr(error)[:512])
                    self.close_connection=True
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Model);server.daemon_threads=True
        thread=threading.Thread(target=server.serve_forever,kwargs={'poll_interval':.05},daemon=True);thread.start()
        env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(empty),ROOK_LOG='error',RUST_BACKTRACE='0',ROOK_QUOTED_FIXTURE_KEY=secret)
        config=f'''skill_sources=[{json.dumps(source.as_posix())}]
[agent]
model='fixture'
install_servers=false
plan_first=false
one_script=false
max_steps=3
max_turn_secs=45
[memory]
enabled=false
[models.fixture]
api='{api}'
model='owned-quoted-model'
context_window=131072
key='env:ROOK_QUOTED_FIXTURE_KEY'
url='http://127.0.0.1:{server.server_port}{'/v1' if api=='openai' else ''}'
'''
        (home/'config.toml').write_text(config,encoding='utf-8')
        def invoke(label,arguments,timeout=50):
            out=case/(label+'.stdout');err=case/(label+'.stderr')
            with out.open('xb') as stdout,err.open('xb') as stderr:
                process=subprocess.run([str(bins['rook']),'-C',str(workspace),*arguments],cwd=workspace,env=env,stdin=subprocess.DEVNULL,
                    stdout=stdout,stderr=stderr,timeout=timeout,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            assert out.stat().st_size<=1024*1024 and err.stat().st_size<=1024*1024
            return process.returncode,out.read_bytes(),err.read_bytes()
        daemon=None;daemon_log=None
        try:
            if mode=='daemon':
                daemon_log=(case/'daemon.log').open('xb')
                daemon=subprocess.Popen([str(bins['rookd']),'-C',str(workspace),'--bind','127.0.0.1','--port','0'],cwd=workspace,env=env,
                    stdin=subprocess.DEVNULL,stdout=daemon_log,stderr=subprocess.STDOUT,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
                deadline=time.monotonic()+20
                while time.monotonic()<deadline:
                    assert daemon.poll() is None,'private daemon exited before readiness'
                    try:
                        address=(home/'rookd.addr').read_text('utf-8').strip();assert address.startswith('http://127.0.0.1:')
                        with opener.open(address+'/api/health',timeout=1) as response:assert response.status==200
                        break
                    except (OSError,AssertionError):time.sleep(.05)
                else:raise AssertionError('owned daemon did not become ready')
            code,out,err=invoke('run',['run','Find the supplied skill query, then reply with the result.'])
            ls_code,ls_out,_=invoke('sessions',['--json','session','ls']);assert ls_code==0
            roots=[row for row in json.loads(ls_out) if not row.get('parent')];assert len(roots)==1
            session=roots[0]['id']
            show_code,show_out,_=invoke('transcript',['--json','session','show',session,'--max-body','65536','--limit','100']);assert show_code==0
            entries=json.loads(show_out)
            calls=[row for row in entries if row['kind']=='tool-call' and row['label']=='find_skill']
            results=[row for row in entries if row['kind']=='tool-result' and row['label']=='find_skill']
            args_seen=[];results_seen=[]
            for messages in later:
                for message in messages:
                    for call in message.get('tool_calls',[]):
                        if call.get('function',{}).get('name')=='find_skill':args_seen.append(json.loads(call['function']['arguments']))
                    if message.get('role')=='tool':results_seen.append(message.get('content',''))
                    if isinstance(message.get('content'),list):
                        for block in message['content']:
                            if block.get('type')=='tool_use' and block.get('name')=='find_skill':args_seen.append(block['input'])
                            if block.get('type')=='tool_result':results_seen.append(json.dumps(block.get('content',''),ensure_ascii=False))
            checks={'client_exit_success':code==0 and b'OWNED_QUOTED_KEY_OK' in out,
                'original_credential_header':len(observations)>=2 and all(row['original_credential_header'] for row in observations),
                'exact_two_main_requests':main_count==2 and 0<=aux_count<=2,
                'correct_wire_route_model':bool(observations) and all(row['path']==('/v1/chat/completions' if api=='openai' else '/v1/messages') and row['model']=='owned-quoted-model' for row in observations),
                'continuation_call_masked':bool(args_seen) and all(value=={'query':'${secret}'} for value in args_seen),
                'continuation_result_masked':bool(results_seen) and all('${secret}' in text and not has_value(text) for text in results_seen),
                'later_messages_masked':bool(later) and not has_value(later),
                'saved_call_masked':len(calls)==1 and calls[0]['truncated'] is False and json.loads(calls[0]['body'])=={'query':'${secret}'},
                'saved_result_masked':len(results)==1 and results[0]['truncated'] is False and '${secret}' in results[0]['body'] and not has_value(results[0]['body']),
                'workspace_unchanged':not any(workspace.iterdir())}
            report['cases'][name]={'checks':checks,'session':session,'client_exit':code,'main_requests':main_count,'auxiliary_requests':aux_count,
                'requests':observations,'continuation_arguments':args_seen,'continuation_results':results_seen,
                'saved_calls':calls,'saved_results':results,'transcript':str(case/'transcript.stdout'),'transcript_sha256':sha(case/'transcript.stdout')}
            report['checks'].update({name+'_'+key:value for key,value in checks.items()})
            print(json.dumps({'case':name,'checks':checks}),flush=True)
            if daemon is not None:
                stop_code,_,_=invoke('stop',['daemon','stop']);assert stop_code==0 and daemon.wait(timeout=10)==0
                retired+=1;daemon=None
        finally:
            if daemon is not None:
                if daemon.poll() is None:daemon.terminate()
                daemon.wait(timeout=10)
            if daemon_log is not None:daemon_log.close()
            server.shutdown();server.server_close();thread.join(timeout=2);assert not thread.is_alive()
    report['checks'].update(binary_pair_unchanged=all(sha(bins[name])==row['sha256'] for name,row in binary_identity.items()),
        no_model_fixture_errors=not report['server_errors'],private_daemon_retired=retired==2)
    report['status']='passed' if all(value is True for value in report['checks'].values()) else 'failed'
finally:
    destination.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'status':report['status'],'conditions':len(report['checks']),'report':str(destination)}),flush=True)
raise SystemExit(0 if report['status']=='passed' else 1)
