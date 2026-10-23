"""Owned local/daemon checks for complete and partial skill-source searches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--tmp-root',type=Path)
args=parser.parse_args();assert __debug__
output=args.output.resolve();assert output.parent.is_dir() and not output.exists()
root=Path(tempfile.mkdtemp(prefix='rook-skill-diagnostics-',dir=args.tmp_root)).resolve()
version=tomllib.loads((Path(__file__).resolve().parents[3]/'Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version']
suffix='.exe' if os.name=='nt' else ''
bins={name:args.bin_dir.resolve()/(name+suffix) for name in ['rook','rookd']}
sha=lambda path:hashlib.sha256(Path(path).read_bytes()).hexdigest()
report={'status':'running','owned_root':str(root),'script_sha256':sha(__file__),'binaries':{},'cases':{},'checks':{}}
source=root/'source';skill=source/'owned-skill';skill.mkdir(parents=True)
(skill/'SKILL.md').write_text('---\nname: owned-skill\ndescription: owned diagnostic fixture\n---\nUse this owned fixture.\n',encoding='utf-8')
missing=root/'missing-source';empty=root/'empty-skills';empty.mkdir()
daemon=None;daemon_log=None;retired=[]
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))

def invoke(case,env,workspace,label,arguments):
    stdout=case/(label+'.stdout');stderr=case/(label+'.stderr')
    with stdout.open('xb') as out,stderr.open('xb') as err:
        result=subprocess.run([str(bins['rook']),'-C',str(workspace),*arguments],env=env,cwd=workspace,
                              stdin=subprocess.DEVNULL,stdout=out,stderr=err,timeout=25,
                              creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert stdout.stat().st_size<=1024*1024 and stderr.stat().st_size<=1024*1024
    return result.returncode,stdout.read_bytes(),stderr.read_bytes()

try:
    for name,binary in bins.items():
        actual=subprocess.check_output([str(binary),'--version'],timeout=10).decode().strip()
        assert actual==name+' '+version
        report['binaries'][name]={'path':str(binary),'version':actual,'sha256':sha(binary)}
    for mode in ['local','daemon']:
        for scenario in ['mixed','unavailable','empty']:
            name=mode+'_'+scenario;case=root/name;case.mkdir();home=case/'home';home.mkdir();workspace=case/'workspace';workspace.mkdir()
            sources=[str(source)] if scenario=='empty' else [str(source),str(missing)] if scenario=='mixed' else [str(missing)]
            (home/'config.toml').write_text('skill_sources = '+json.dumps(sources)+'\n[agent]\ninstall_servers=false\n[memory]\nenabled=false\n',encoding='utf-8')
            env=dict(os.environ,ROOK_HOME=str(home),ROOK_CONFIG_DIR=str(home),ROOK_BUILTIN_SKILLS=str(empty),
                     ROOK_LOG='error',GIT_CONFIG_GLOBAL=str(case/'empty.gitconfig'),GIT_CONFIG_NOSYSTEM='1')
            base=None
            if mode=='daemon':
                daemon_log=(case/'daemon.log').open('xb')
                daemon=subprocess.Popen([str(bins['rookd']),'-C',str(workspace),'--bind','127.0.0.1','--port','0'],
                                        env=env,cwd=workspace,stdin=subprocess.DEVNULL,stdout=daemon_log,stderr=subprocess.STDOUT,
                                        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
                deadline=time.monotonic()+20
                while time.monotonic()<deadline:
                    assert daemon.poll() is None,'private daemon exited before readiness'
                    try:
                        base=(home/'rookd.addr').read_text(encoding='utf-8').strip()
                        assert base.startswith('http://127.0.0.1:')
                        with opener.open(base+'/api/health',timeout=1) as response:assert response.status==200
                        break
                    except (OSError,AssertionError):time.sleep(.05)
                else:raise AssertionError('private daemon did not become ready')
            query='platypusquartz' if scenario=='empty' else 'owned'
            code,raw,err=invoke(case,env,workspace,'legacy',['skills','search',query,'--json'])
            assert code==0,(code,raw,err)
            legacy=json.loads(raw)
            code,raw,err=invoke(case,env,workspace,'diagnostics',['skills','search',query,'--json','--diagnostics'])
            assert code==0,(code,raw,err)
            diagnostic=json.loads(raw);assert set(diagnostic)=={'items','errors'}
            code,raw,err=invoke(case,env,workspace,'plain',['skills','search',query])
            assert code==0,(code,raw,err)
            plain=raw.decode('utf-8')
            expected_items=1 if scenario=='mixed' else 0
            expected_errors=0 if scenario=='empty' else 1
            checks={'legacy_array_and_diagnostic_items_match':isinstance(legacy,list) and legacy==diagnostic['items'],
                    'expected_matches_preserved':len(legacy)==expected_items and all(set(item)=={'name','description','source'} and item['name']=='owned-skill' and item['source']==str(source) and item['description']=='owned diagnostic fixture' for item in legacy),
                    'source_failure_distinguished':len(diagnostic['errors'])==expected_errors and all(str(missing) in error for error in diagnostic['errors']),
                    'plain_search_semantics':('incomplete' in plain and 'nothing offered matches' not in plain) if scenario=='unavailable' else ('owned-skill' in plain and str(missing) in plain) if scenario=='mixed' else 'nothing offered matches' in plain}
            assert all(value is True for value in checks.values()),checks
            if mode=='daemon':
                assert daemon.poll() is None
                with opener.open(base+'/api/skills/offered?q='+query,timeout=10) as response:
                    data=response.read(1024*1024+1);assert len(data)<=1024*1024
                    remote=json.loads(data)
                assert len(remote['items'])==expected_items and remote['errors']==diagnostic['errors']
                code,raw,err=invoke(case,env,workspace,'stop',['daemon','stop']);assert code==0,(code,raw,err)
                assert daemon.wait(timeout=10)==0
                retired.append(daemon.pid);daemon=None;daemon_log.close();daemon_log=None
            report['cases'][name]={'checks':checks,'legacy':legacy,'diagnostic':diagnostic,'daemon_address':base}
            report['checks'].update({name+'_'+key:value for key,value in checks.items()})
            print(json.dumps({'case':name,'checks':checks}),flush=True)
    code,raw,err=invoke(case,env,workspace,'requires-json',['skills','search','--diagnostics'])
    report['checks']['diagnostics_requires_json']=code==2 and b'--json' in err and raw==b''
    report['checks']['three_private_daemons_retired']=len(retired)==3
    report['checks']['same_binary_pair_unchanged']=all(sha(bins[name])==row['sha256'] for name,row in report['binaries'].items())
    assert all(value is True for value in report['checks'].values()),report['checks']
    report['status']='passed'
finally:
    if daemon is not None:
        if daemon.poll() is None:daemon.kill()
        daemon.wait(timeout=10)
    if daemon_log is not None:daemon_log.close()
    output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'status':report['status'],'checks':len(report['checks']),'report':str(output)}),flush=True)
