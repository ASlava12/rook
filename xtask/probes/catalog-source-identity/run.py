"""Owned smart-HTTP source identity through search, installation and update."""
import argparse
import tempfile
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import urllib.parse

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
homes=parser.add_mutually_exclusive_group()
homes.add_argument('--long-home',action='store_true',help='Exercise a deeply nested private Windows catalog home')
homes.add_argument('--medium-home',action='store_true',help='Exercise long Git pack paths below the config bootstrap limit')
args=parser.parse_args()
assert not (args.long_home or args.medium_home) or os.name=='nt','nested-home fixtures are specific to Git for Windows'
suffix='.exe' if os.name=='nt' else ''
binary=args.bin_dir.resolve()/('rook'+suffix)
report_path=args.output.resolve()
compiler_git=shutil.which('git')
if not __debug__ or not binary.is_file() or not compiler_git or report_path.exists() or not report_path.parent.is_dir():
    parser.error('Unoptimized Python, actual CLI, Git and fresh output path required')
git=os.path.abspath(compiler_git)
parent=(args.tmp_root or Path(tempfile.gettempdir())).resolve()
root=Path(tempfile.mkdtemp(prefix='rook-catalog-identity-',dir=parent)).resolve()
assert root.parent==parent and root.name.startswith('rook-catalog-identity-')
for name in ['http-root','home','workspace','empty']: (root/name).mkdir(exist_ok=False)
home=root/'home'
if args.long_home:
    home=home/('nested-'+('x'*50))
    home=home/('nested-'+('y'*max(50,174-len(str(home))-8)))
    home.mkdir(parents=True)
elif args.medium_home:
    assert len(str(home))<129,'Use a shorter private temporary root for the medium-home fixture'
    home=home/('m'*(129-len(str(home))))
    home.mkdir(parents=True)
http_root=root/'http-root'
(http_root/'a').mkdir()
empty_config=root/'empty-git-config';empty_config.write_bytes(b'')
isolated_env={key:value for key,value in os.environ.items() if not key.upper().startswith(('GIT_','ROOK_'))}
git_env=dict(isolated_env,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL=str(empty_config),GIT_CONFIG_COUNT='0',GIT_TERMINAL_PROMPT='0',
    HTTP_PROXY='',HTTPS_PROXY='',ALL_PROXY='',http_proxy='',https_proxy='',all_proxy='',NO_PROXY='127.0.0.1,localhost')
def run(command,cwd,**kwargs):
    # Fixture preparation may cross MAX_PATH too. Keep this option local to
    # these setup/inspection commands; the tested CLI gets git_env unchanged.
    if args.long_home or args.medium_home:
        assert command[0]==git
        command=[git,'-c','core.longpaths=true',*command[1:]]
    return subprocess.run(command,cwd=cwd,env=git_env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
        timeout=30,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0),check=True,**kwargs)
for number,name,path in [(1,'first-owned',http_root/'a/b'),(2,'second-owned',http_root/'a-b'),
                         (3,'origin-owned',http_root/'a/b.git')]:
    source=root/f'source{number}';source.mkdir()
    card=source/'skills'/name/'SKILL.md';card.parent.mkdir(parents=True)
    card.write_bytes(f'---\nname: {name}\ndescription: Owned collision diagnostic repository {number}\n---\n\nOwned identity {number}.\n'.encode())
    if number in [1,3]:
        origin=source/'skills/origin-owned';origin.mkdir(exist_ok=True)
        (origin/'SKILL.md').write_bytes(f'---\nname: origin-owned\ndescription: Owned collision origin {number}\n---\n\nOwned origin {number}, revision 1.\n'.encode())
        (origin/'proof.txt').write_bytes(f'origin {number} revision 1\n'.encode())
    run([git,'init'],source)
    run([git,'add','--','skills'],source)
    run([git,'-c','user.name=Owned diagnostic','-c','user.email=owned@example.invalid','commit','-m','Owned skill identity'],source)
    run([git,'clone','--bare',str(source),str(path)],root)

requests=[]
errors=[]
refuse_refresh=False
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self): self.serve()
    def do_POST(self): self.serve()
    def serve(self):
        try:
            parsed=urllib.parse.urlsplit(self.path)
            assert parsed.path.endswith(('/info/refs','/git-upload-pack'))
            assert len(requests)<64
            requests.append({'method':self.command,'path':parsed.path})
            if refuse_refresh:
                self.send_error(503,'Owned refresh refusal')
                return
            size=int(self.headers.get('Content-Length','0'));assert 0<=size<=65536
            body=self.rfile.read(size)
            env=dict(git_env,GIT_PROJECT_ROOT=str(http_root),GIT_HTTP_EXPORT_ALL='1',PATH_INFO=parsed.path,
                QUERY_STRING=parsed.query,REQUEST_METHOD=self.command,CONTENT_TYPE=self.headers.get('Content-Type',''),
                CONTENT_LENGTH=str(size),SERVER_PROTOCOL='HTTP/1.1',REMOTE_ADDR='127.0.0.1')
            if self.headers.get('Git-Protocol'):env['HTTP_GIT_PROTOCOL']=self.headers['Git-Protocol']
            result=subprocess.run([git,'http-backend'],input=body,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=20,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0),check=True)
            assert len(result.stdout)<=1024*1024
            raw=result.stdout
            separator=b'\r\n\r\n' if b'\r\n\r\n' in raw else b'\n\n'
            header,payload=raw.split(separator,1)
            headers=[line.split(':',1) for line in header.decode('ascii').splitlines()]
            status=next((int(value.strip().split()[0]) for key,value in headers if key.lower()=='status'),200)
            self.send_response(status)
            for key,value in headers:
                if key.lower() not in ['status','content-length','connection']: self.send_header(key,value.strip())
            self.send_header('Content-Length',str(len(payload)));self.end_headers();self.wfile.write(payload)
        except Exception as error:
            if len(errors)<8: errors.append(repr(error)[:500])
            self.send_error(500)

server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
urls=[f'http://127.0.0.1:{server.server_port}'+path for path in ['/a/b','/a-b','/a/b.git']]
def cache_name(value):
    value=value.rstrip('/')
    while value.endswith('.git'):value=value[:-4]
    return ''.join(char if char.isascii() and (char.isalnum() or char in '-.') else '-' for char in value).strip('-.') or 'source'
assert urls[0]!=urls[1] and cache_name(urls[0])==cache_name(urls[1])
(home/'config.toml').write_text('skill_sources='+json.dumps(urls)+'\n[agent]\ninstall_servers=false\n',encoding='utf-8')
env=dict(git_env,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(root/'empty'))
def sha(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()
identity=sha(binary)
report={'status':'incomplete','binary':str(binary),'binary_sha256':identity,'owned_directory':str(root),'sources':urls,
    'long_home':args.long_home,'medium_home':args.medium_home,'home':str(home),'home_characters':len(str(home)),
    'script_sha256':sha(Path(__file__).resolve()),'git_version':run([git,'--version'],root).stdout.decode().strip()}
def snapshot(directory):
    return {path.relative_to(directory).as_posix():sha(path) for path in directory.rglob('*') if path.is_file()}
def cli(label,words,json_mode=True):
    command=[str(binary),'--workspace',str(root/'workspace'),*(['--json'] if json_mode else []),'skills',*words]
    result=subprocess.run(command,env=env,cwd=root/'workspace',stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=45,
        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert len(result.stdout)<=65536 and len(result.stderr)<=65536
    (root/(label+'.stdout')).write_bytes(result.stdout)
    (root/(label+'.stderr')).write_bytes(result.stderr)
    assert result.returncode==0,(label,result.returncode,result.stderr[-2000:])
    return result.stdout

def search(label,refresh=False):
    offers=json.loads(cli(label,['search','collision',*(['--refresh'] if refresh else [])]))
    assert isinstance(offers,list)
    return offers
def correct(offers):
    return len(offers)==4 and {(row['name'],row['source']) for row in offers}=={
        ('first-owned',urls[0]),('origin-owned',urls[0]),('second-owned',urls[1]),('origin-owned',urls[2])}
try:
    cache=home/'cache/sources';cache.mkdir(parents=True)
    legacy=cache/cache_name(urls[0])
    run([git,'clone','--',urls[0],str(legacy)],root)
    legacy_before=snapshot(legacy)
    initial_count=len(requests)
    first=search('cold')
    cold_requests=list(requests[initial_count:])
    before_warm=len(requests)
    warm=search('warm')
    warm_requests=list(requests[before_warm:])
    before_refresh=len(requests)
    refreshed=search('refresh',True)
    refresh_requests=list(requests[before_refresh:])
    wanted=root/'source3/skills/origin-owned'
    expected_install={name:(wanted/name).read_bytes() for name in ['SKILL.md','proof.txt']}
    cli('install',['install','origin-owned','--source',urls[2]])
    installed=home/'skills/origin-owned'
    install_correct=all((installed/name).is_file() and (installed/name).read_bytes()==value for name,value in expected_install.items())
    installed_hashes={name:sha(installed/name) for name in expected_install if (installed/name).is_file()}
    for name in expected_install:
        (wanted/name).write_bytes(expected_install[name].replace(b'revision 1',b'revision 2'))
    run([git,'add','--','skills'],root/'source3')
    run([git,'-c','user.name=Owned diagnostic','-c','user.email=owned@example.invalid','commit','-m','Owned origin revision two'],root/'source3')
    branch=run([git,'branch','--show-current'],root/'source3').stdout.decode().strip()
    run([git,'push','--',str(http_root/'a/b.git'),'HEAD:refs/heads/'+branch],root/'source3')
    cli('update',['update'])
    update_correct=all((installed/name).is_file() and (installed/name).read_bytes()==(wanted/name).read_bytes() for name in expected_install)
    updated_hashes={name:sha(installed/name) for name in expected_install if (installed/name).is_file()}
    entries=[path for path in cache.iterdir() if path.is_dir() and path!=legacy]
    origins=[]
    for directory in entries:
        config=directory/'.git/config'
        raw=config.read_bytes();assert len(raw)<=65536
        if args.long_home or args.medium_home:
            # Git cannot bootstrap inside this long directory. Parse the
            # actual cached config bytes through Git's stdin configuration
            # reader, keeping the source-origin assertion independent of
            # Git's path-length problem in the inspection helper itself.
            result=subprocess.run([git,'config','--file','-','--get','remote.origin.url'],input=raw,
                cwd=root,env=git_env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=30,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0),check=True)
        else:
            result=run([git,'-C',str(directory),'config','--get','remote.origin.url'],root)
        assert len(result.stdout)<=65536 and len(result.stderr)<=65536
        origins.append({'directory':directory.name,'origin':result.stdout.decode('utf-8').strip(),
            'config_sha256':hashlib.sha256(raw).hexdigest(),
            'config_utf16_units':len(str(config).encode('utf-16-le'))//2,
            'pack_path_units':[len(str(path).encode('utf-16-le'))//2 for path in (directory/'.git/objects/pack').iterdir()]})
    if args.medium_home:
        assert all(entry['config_utf16_units']<240 and max(entry['pack_path_units'])>=260 for entry in origins)
    if args.long_home:
        assert all(entry['config_utf16_units']>=260 for entry in origins)
    def reached_all(rows):
        return all(any(row['path'].startswith(path+'/') for row in rows) for path in ['/a/b','/a-b','/a/b.git'])
    report['checks']={
        'distinct_sources_offer_their_own_skills':correct(first),
        'all_remote_repositories_physically_fetched':reached_all(cold_requests),
        'separate_cache_entries_retain_correct_origins':len(entries)==3 and {row['origin'] for row in origins}==set(urls),
        'warm_search_reuses_the_correct_caches':correct(warm) and not warm_requests,
        'refresh_preserves_all_source_identities':correct(refreshed) and reached_all(refresh_requests),
        'legacy_lossy_cache_unchanged':legacy.is_dir() and snapshot(legacy)==legacy_before,
        'workspace_unchanged':not list((root/'workspace').iterdir()),
        'binary_unchanged':sha(binary)==identity,
        'explicit_origin_installs_exact_payload':install_correct,
        'update_keeps_the_recorded_exact_origin':update_correct,
    }
    if args.long_home:
        before_failure=snapshot(cache)
        refuse_refresh=True
        refused=search('refused-refresh',True)
        after_failure=snapshot(cache)
        refuse_refresh=False
        before_reuse=len(requests)
        recovered=search('reuse-after-refusal')
        report['checks'].update(
            failed_refresh_retains_exact_previous_cache=not refused and after_failure==before_failure,
            cache_reusable_without_network_after_failed_refresh=correct(recovered) and len(requests)==before_reuse)
        alternate=urls[0]+'/'
        unexpected=cache/('b-'+hashlib.sha256(alternate.encode()).hexdigest())
        unexpected.mkdir();(unexpected/'sentinel.txt').write_bytes(b'owned non-cache content')
        previous=snapshot(unexpected)
        config_path=home/'config.toml';config_before=config_path.read_bytes()
        before_refusal=len(requests)
        try:
            config_path.write_text('skill_sources='+json.dumps([alternate])+'\n[agent]\ninstall_servers=false\n',encoding='utf-8')
            unknown=search('non-cache-target',True)
        finally:
            config_path.write_bytes(config_before)
        report['checks']['unexpected_cache_directory_retained']=not unknown and snapshot(unexpected)==previous and len(requests)==before_refusal
        report['unexpected_cache_directory']=str(unexpected)
        first_cache=next(cache/entry['directory'] for entry in origins if entry['origin']==urls[0])
        original_cache=snapshot(first_cache)
        oversized=root/'source1/owned-oversize.bin'
        oversized.write_bytes(b'owned-copy-budget\n'*(9*1024*1024//18+1))
        run([git,'add','--',oversized.name],root/'source1')
        run([git,'-c','user.name=Owned diagnostic','-c','user.email=owned@example.invalid','commit','-m','Owned copy ceiling'],root/'source1')
        first_branch=run([git,'branch','--show-current'],root/'source1').stdout.decode().strip()
        run([git,'push','--',str(http_root/'a/b'),'HEAD:refs/heads/'+first_branch],root/'source1')
        try:
            config_path.write_text('skill_sources='+json.dumps([urls[0]])+'\n[agent]\ninstall_servers=false\n',encoding='utf-8')
            rejected_copy=search('copy-budget-refresh',True)
            copy_diagnostic=cli('copy-budget-diagnostic',['search','collision','--refresh'],json_mode=False)
        finally:
            config_path.write_bytes(config_before)
        report['checks']['copy_budget_failure_retains_previous_cache']=not rejected_copy and snapshot(first_cache)==original_cache and not (home/'skills/first-owned').exists() and b'bounded regular file' in copy_diagnostic
        report.update(copy_budget_file_bytes=oversized.stat().st_size,copy_budget_file_sha256=sha(oversized),
            copy_budget_diagnostic=copy_diagnostic.decode('utf-8'))
    report.update(status='passed' if all(report['checks'].values()) and not errors else 'failed',
        cold_offers=first,warm_offers=warm,refreshed_offers=refreshed,cache_origins=origins,
        legacy_directory=str(legacy),legacy_file_count=len(legacy_before),
        cold_requests=cold_requests,warm_requests=warm_requests,refresh_requests=refresh_requests,
        explicit_install_origin=urls[2],installed_payload_sha256=installed_hashes,
        expected_install_sha256={name:hashlib.sha256(value).hexdigest() for name,value in expected_install.items()},
        updated_payload_sha256=updated_hashes,expected_update_sha256={name:sha(wanted/name) for name in expected_install},
        scope='Owned smart-HTTP Git repositories and fresh private CLI state. All diagnostic files and new private caches are retained; no cache is removed or migrated. No installed daemon, user cache or installation changed.')
    assert not errors,errors
    assert all(report['checks'].values()),report['checks']
finally:
    server.shutdown();server.server_close()
    report['http_requests']=requests;report['server_errors']=errors
    with report_path.open('x',encoding='utf-8') as output:json.dump(report,output,indent=2)
    print(json.dumps(report),flush=True)
