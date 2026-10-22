"""Actual CLI workspace admission before PATH language-server side effects."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
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
compiler=shutil.which('rustc')
assert __debug__ and binary.is_file() and compiler and not args.output.exists() and args.output.parent.is_dir()
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
root=Path(tempfile.mkdtemp(prefix='rook-lsp-workspace-',dir=parent))
known=['rust-analyzer','typescript-language-server','pyright-langserver','gopls','clangd']
inventory=['rustc','python3','node','go','java','ruby','php','dotnet','git','cargo','npm','uv','docker','kubectl','rg','gh']
for name in ['tools','empty-skills']: (root/name).mkdir()
helper=root/'tools'/('rust-analyzer'+suffix)
subprocess.run([compiler,str(Path(__file__).with_name('helper.rs')),'--edition','2024','-C','opt-level=s','-C','debuginfo=0','-o',str(helper)],check=True,timeout=90)
for name in inventory+known[1:]:
    target=root/'tools'/(name+suffix)
    shutil.copy2(helper,target);target.chmod(target.stat().st_mode|0o111)

def sha(path):
    with path.open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()
def snapshot(directory):
    return {p.relative_to(directory).as_posix():sha(p) for p in directory.rglob('*') if p.is_file()}

requests=[]
class HTTP(http.server.BaseHTTPRequestHandler):
    def log_message(self,*unused): pass
    def do_GET(self):
        requests.append(('GET',self.path));self.send_error(500)
    def do_POST(self):
        requests.append(('POST',self.path));self.send_error(500)
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),HTTP)
threading.Thread(target=server.serve_forever,daemon=True).start()
identity=sha(binary)
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'owned_directory':str(root),'cases':[]}
scenarios=[
    ('empty',{'readme.txt':b'owned notes'},[],None,None),
    ('rust',{'main.rs':b'fn main() {}'},['rust-analyzer'],None,'rust'),
    ('python',{'main.py':b'print(1)'},['pyright-langserver'],None,'python'),
    ('mixed',{'main.rs':b'','main.ts':b'','main.py':b'','main.go':b'','main.c':b''},known,None,'rust, typescript, python, go, c'),
    ('explicit',{'readme.txt':b'owned notes'},[],None,'notes'),
    ('refusal',{'main.py':b'print(1)'},['pyright-langserver'],'pyright-langserver',None),
]
try:
    for name,files,expected,refuse,offered in scenarios:
        case=root/name;case.mkdir()
        home=case/'home';home.mkdir()
        workspace=case/'workspace';workspace.mkdir()
        for filename,data in files.items(): (workspace/filename).write_bytes(data)
        before=snapshot(workspace)
        config=f"[agent]\nmodel='fixture'\ninstall_servers=false\n[models.fixture]\napi='openai'\nmodel='owned-workspace'\ncontext_window=32768\nurl='http://127.0.0.1:{server.server_port}/v1'\n"
        if name=='explicit': config+="[[lsp]]\nlanguage='notes'\ncommand='never-started'\nextensions=['txt']\n"
        (home/'config.toml').write_text(config,encoding='utf-8')
        log=case/'calls.txt'
        env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_LOG='error',
            ROOK_BUILTIN_SKILLS=str(root/'empty-skills'),PATH=str(root/'tools'),
            ROOK_OWNED_LSP_WORKSPACE_LOG=str(log),ROOK_OWNED_LSP_WORKSPACE_REFUSE=refuse or '')
        started=time.monotonic()
        result=subprocess.run([str(binary),'--workspace',str(workspace),'lsp','servers'],env=env,cwd=workspace,
            stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=45,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        assert len(result.stdout)<=65536 and len(result.stderr)<=65536
        calls=[line.split() for line in log.read_text().splitlines()] if log.exists() else []
        assert all(len(row)==3 for row in calls),calls
        report['cases'].append({'name':name,'calls':calls,'expected_calls':expected,'seconds':time.monotonic()-started,
            'exit_code':result.returncode,'stdout':result.stdout.decode('utf-8',errors='replace'),
            'stderr':result.stderr.decode('utf-8',errors='replace')[-2000:],
            'correct_result':(result.returncode==0 and result.stdout.decode().strip()==offered) if offered is not None else
                (result.returncode!=0 and b'no language server applies' in result.stderr),
            'workspace_unchanged':snapshot(workspace)==before})
    checks={case['name']+'_probes_only_applicable_paths':[row[0] for row in case['calls']]==case['expected_calls'] for case in report['cases']}
    checks.update(correct_results=all(c['correct_result'] for c in report['cases']),
        stdin_closed=all(row[1]=='true' for c in report['cases'] for row in c['calls']),
        only_version_requests=all(row[2]=='true' for c in report['cases'] for row in c['calls']),
        no_model_http=not requests,workspace_unchanged=all(c['workspace_unchanged'] for c in report['cases']),binary_unchanged=sha(binary)==identity)
    assert len(checks)==12
    report['checks']=checks
    report['status']='passed' if all(checks.values()) else 'failed'
finally:
    server.shutdown();server.server_close()
    report['http_requests']=requests
    with args.output.open('x',encoding='utf-8') as sink: json.dump(report,sink,indent=2)
    print(json.dumps(report),flush=True)
assert report['status']=='passed',report.get('checks')
