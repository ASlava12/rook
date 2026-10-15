# Fixed code over authenticated SSH; all paths and queries arrive as JSON data.
import base64,ctypes,fcntl,hashlib,json,os,selectors,signal,stat,subprocess,sys,time
MAX_REQUEST=1024*1024
raw=sys.stdin.buffer.read(MAX_REQUEST+1)
if len(raw)>MAX_REQUEST:raise SystemExit('native workspace request exceeds its bound')
request=json.loads(raw)
limits=request['limits'];base=request['base'];operation=request['operation']
MAX_DEPTH=64

def valid(path,root=False):
    if root and path in ('','.'):return ''
    if not isinstance(path,str) or not 0<len(path.encode())<=1024 or '\\' in path or any(ord(c)<32 or ord(c)==127 for c in path) or any(p in ('','.','..') for p in path.split('/')):raise RuntimeError('use a bounded path relative to the selected remote root')
    return path

def root_open():
    if not isinstance(base,str) or not base.startswith('/') or len(base.encode())>1024:raise RuntimeError('invalid trusted remote root')
    fd=os.open('/',os.O_RDONLY|os.O_DIRECTORY)
    try:
        for part in base.split('/'):
            if not part:continue
            if part in ('.','..'):raise RuntimeError('invalid trusted remote root component')
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
            os.close(fd);fd=child
        return fd
    except BaseException:os.close(fd);raise

def directory(root,path):
    fd=os.dup(root)
    try:
        for part in path.split('/') if path else []:
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
            os.close(fd);fd=child
        return fd
    except BaseException:os.close(fd);raise

def read(root,path):
    parent,name=path.rsplit('/',1) if '/' in path else ('',path)
    fd=directory(root,parent)
    try:child=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=fd)
    finally:os.close(fd)
    try:
        before=os.fstat(child)
        if not stat.S_ISREG(before.st_mode) or before.st_size>limits['file_bytes']:raise RuntimeError('native source must be a regular file within execution_environment.file_bytes')
        with os.fdopen(child,'rb',closefd=False) as source:data=source.read(limits['file_bytes']+1)
        after=os.fstat(child)
        if len(data)>limits['file_bytes']:raise RuntimeError('native source grew beyond file_bytes')
        if (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns):raise RuntimeError('native source changed during the read')
        return data
    finally:os.close(child)

def read_text(root,args):
    path=valid(args.get('path'));data=read(root,path)
    try:text=data.decode('utf-8')
    except UnicodeDecodeError:raise RuntimeError('native source is not UTF-8 text')
    digest=hashlib.sha256(data).hexdigest();offset=args.get('offset',0)
    if not isinstance(offset,int) or isinstance(offset,bool) or not 0<=offset<=len(data):raise RuntimeError('invalid UTF-8 byte offset')
    if offset and args.get('sha256')!=digest:raise RuntimeError('native source changed or continuation sha256 is missing')
    try:remaining=data[offset:].decode('utf-8')
    except UnicodeDecodeError:raise RuntimeError('offset must be a UTF-8 boundary')
    amount=max(0,(limits['output_bytes']-2048)//6)
    selected=remaining.encode()[:amount]
    while selected:
        try:text=selected.decode();break
        except UnicodeDecodeError:selected=selected[:-1]
    if not selected:text=''
    if not selected and remaining:raise RuntimeError('output budget leaves no room for native text')
    end=offset+len(selected)
    return dict(path=path,sha256=digest,text=text,offset=offset,total_bytes=len(data),next_offset=end if end<len(data) else None)

def listing(root,args):
    path=valid(args.get('path','.'),True);fd=directory(root,path)
    entries=[];after=args.get('after','')
    if not isinstance(after,str) or len(after.encode())>1024:raise RuntimeError('invalid directory cursor')
    try:
        with os.scandir(fd) as stream:
            for count,entry in enumerate(stream,1):
                if count>limits['max_entries']:raise RuntimeError('native directory exceeds max_entries; select a smaller path')
                valid(entry.name);info=entry.stat(follow_symlinks=False)
                kind='directory' if stat.S_ISDIR(info.st_mode) else 'file' if stat.S_ISREG(info.st_mode) else 'symlink' if stat.S_ISLNK(info.st_mode) else 'other'
                if entry.name>after:entries.append(dict(name=entry.name,kind=kind,bytes=info.st_size))
    finally:os.close(fd)
    entries.sort(key=lambda e:e['name']);selected=entries[:8]
    return dict(path=path or '.',entries=selected,after=selected[-1]['name'] if len(entries)>len(selected) else None)

def search(root,args):
    path=valid(args.get('path','.'),True);pattern=args.get('pattern')
    if not isinstance(pattern,str) or not 0<len(pattern.encode())<=512:raise RuntimeError('search requires a bounded literal pattern')
    state=dict(matches=[],scanned_files=0,scanned_bytes=0,entries=0,complete=True)
    def visit(fd,prefix,depth):
        if depth>MAX_DEPTH:state['complete']=False;return
        with os.scandir(fd) as stream:
            for entry in stream:
                state['entries']+=1
                if state['entries']>limits['max_entries']:state['complete']=False;return
                if entry.name in limits['exclude']:continue
                candidate=prefix+entry.name;valid(candidate)
                info=entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    child=os.open(entry.name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
                    try:visit(child,candidate+'/',depth+1)
                    finally:os.close(child)
                elif stat.S_ISREG(info.st_mode):
                    if state['scanned_files']>=limits['max_files'] or info.st_size>limits['file_bytes'] or state['scanned_bytes']+info.st_size>limits['total_bytes']:state['complete']=False;return
                    data=read(root,candidate);state['scanned_files']+=1;state['scanned_bytes']+=len(data)
                    if b'\0' in data:continue
                    try:text=data.decode('utf-8')
                    except UnicodeDecodeError:continue
                    for number,line in enumerate(text.splitlines(),1):
                        if pattern in line:
                            state['matches'].append(dict(path=candidate,line=number,text=line[:128]))
                            if len(state['matches'])>=8:state['complete']=False;return
                if not state['complete']:return
    fd=directory(root,path)
    try:visit(fd,path+'/' if path else '',0)
    finally:os.close(fd)
    return state


MAX_PROCESSES=32768
def process(pid):
    try:
        with open('/proc/' + str(pid) + '/stat', 'rb') as source: data = source.read(4097)
        if len(data) > 4096: raise RuntimeError('process metadata exceeds its bound')
        values = data.rsplit(b')', 1)[1].split()
        return {'pid': pid, 'state': values[0].decode('ascii'), 'parent': int(values[1]), 'group': int(values[2]), 'birth': int(values[19])}
    except (FileNotFoundError, ProcessLookupError): return None

def descendants():
    entries = {}; count = 0
    with os.scandir('/proc') as processes:
        for entry in processes:
            if not entry.name.isdecimal(): continue
            count += 1
            if count > MAX_PROCESSES: raise RuntimeError('process scan bound exceeded')
            try: info = process(int(entry.name))
            except PermissionError: continue
            if info: entries[info['pid']] = info
    owned = {os.getpid()}; changed = True
    while changed:
        added = {pid for pid, info in entries.items() if info['parent'] in owned and pid not in owned}
        changed = bool(added); owned.update(added)
    return [entries[pid] for pid in owned if pid != os.getpid() and entries[pid]['state'] != 'Z']

def signal_owned(info, number):
    initial = process(info['pid'])
    if not initial or initial['birth'] != info['birth']: return False
    try: handle = os.pidfd_open(info['pid'])
    except ProcessLookupError: return True
    try:
        fresh = process(info['pid'])
        if not fresh or fresh['birth'] != info['birth']: return False
        signal.pidfd_send_signal(handle, number)
        return True
    except ProcessLookupError: return True
    finally: os.close(handle)

def cleanup():
    deadline = time.monotonic() + 5
    while True:
        for info in descendants(): signal_owned(info, signal.SIGKILL)
        try:
            while os.waitpid(-1, os.WNOHANG)[0]: pass
        except ChildProcessError: pass
        if not descendants(): return True
        if time.monotonic() >= deadline: return False
        time.sleep(0.02)


def attrs(info):return dict(mode=stat.S_IMODE(info.st_mode),uid=info.st_uid,gid=info.st_gid)

def snapshot_root(root):
    result=dict(files={},dirs={});count=0;total=0
    def visit(fd,prefix,depth):
        nonlocal count,total
        if depth>MAX_DEPTH:raise RuntimeError('native snapshot depth exceeded')
        with os.scandir(fd) as entries:
            for entry in entries:
                count+=1
                if count>limits['max_entries']:raise RuntimeError('native snapshot exceeds max_entries')
                if entry.name in limits['exclude']:continue
                path=prefix+entry.name;valid(path);info=entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    result['dirs'][path]=attrs(info)
                    child=os.open(entry.name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
                    try:visit(child,path+'/',depth+1)
                    finally:os.close(child)
                elif stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
                    if len(result['files'])>=limits['max_files']:raise RuntimeError('native snapshot exceeds max_files')
                    if stat.S_ISLNK(info.st_mode):
                        target=os.readlink(entry.name,dir_fd=fd)
                        if len(target.encode())>1024:raise RuntimeError('native symlink target exceeds its byte bound')
                        item=dict(kind='link',target=target,attrs=attrs(info));total+=len(target.encode())
                    else:
                        data=read(root,path);total+=len(data)
                        item=dict(kind='file',data=base64.b64encode(data).decode(),attrs=attrs(info))
                    if total>limits['total_bytes']:raise RuntimeError('native snapshot exceeds total_bytes')
                    result['files'][path]=item
                else:raise RuntimeError('native snapshot found a special file; select explicit excluded entry names before execution')
    visit(root,'',0);return result

def snapshot_hash(snapshot):return hashlib.sha256(json.dumps(snapshot,sort_keys=True,separators=(',',':'),ensure_ascii=True).encode()).hexdigest()

def put_item(root,path,item):
    if item['kind']=='file':replace_source(root,path,base64.b64decode(item['data'],validate=True),item['attrs']);return
    parent,name=path.rsplit('/',1) if '/' in path else ('',path);fd=directory(root,parent);temporary='.rook-native-'+control['call']
    try:
        os.symlink(item['target'],temporary,dir_fd=fd)
        os.chown(temporary,item['attrs']['uid'],item['attrs']['gid'],dir_fd=fd,follow_symlinks=False)
        os.replace(temporary,name,src_dir_fd=fd,dst_dir_fd=fd);os.fsync(fd)
    finally:
        try:os.unlink(temporary,dir_fd=fd)
        except FileNotFoundError:pass
        os.close(fd)

def restore_root(root,current,before):
    # Snapshot data stays in the owned checkpoint; no local replica is uploaded.
    for path in sorted(set(current['files'])-set(before['files']),key=lambda p:(-p.count('/'),p)):
        parent,name=path.rsplit('/',1) if '/' in path else ('',path);fd=directory(root,parent)
        try:os.unlink(name,dir_fd=fd)
        finally:os.close(fd)
    for path in sorted(set(current['dirs'])-set(before['dirs']),key=lambda p:(-p.count('/'),p)):
        parent,name=path.rsplit('/',1) if '/' in path else ('',path);fd=directory(root,parent)
        try:os.rmdir(name,dir_fd=fd)
        finally:os.close(fd)
    for path in sorted(set(before['dirs'])-set(current['dirs']),key=lambda p:(p.count('/'),p)):
        parent,name=path.rsplit('/',1) if '/' in path else ('',path);fd=directory(root,parent)
        try:os.mkdir(name,0o700,dir_fd=fd)
        finally:os.close(fd)
    for path,item in sorted(before['files'].items()):
        if current['files'].get(path)!=item:put_item(root,path,item)
    for path,attributes in sorted(before['dirs'].items(),key=lambda item:(-item[0].count('/'),item[0])):
        fd=directory(root,path)
        try:
            info=os.fstat(fd)
            if (info.st_uid,info.st_gid)!=(attributes['uid'],attributes['gid']):os.fchown(fd,attributes['uid'],attributes['gid'])
            os.fchmod(fd,attributes['mode']);os.fsync(fd)
        finally:os.close(fd)
    if snapshot_hash(snapshot_root(root))!=snapshot_hash(before):raise RuntimeError('native undo verification differs; preserve checkpoint and inspect partial recovery')

def run_owned(root,args,record,save):
    if sys.platform!='linux' or not hasattr(os,'pidfd_open') or not hasattr(signal,'pidfd_send_signal'):raise RuntimeError('native execution requires Linux with pidfd and Python 3.9+')
    if ctypes.CDLL(None).prctl(36,1,0,0,0)!=0:raise RuntimeError('cannot own adopted remote descendants')
    cwd=valid(args.get('path','.'),True);fd=directory(root,cwd);os.close(fd)
    command=args.get('command')
    if not isinstance(command,str) or not 0<len(command.encode())<=128*1024:raise RuntimeError('native command exceeds its byte bound')
    interrupted=[False]
    def interrupt(_number,_frame):interrupted[0]=True
    for number in (signal.SIGTERM,signal.SIGHUP,signal.SIGINT):signal.signal(number,interrupt)
    env=dict(os.environ)
    for key,value in request.get('environment',{}).items():
        if not isinstance(key,str) or not key.startswith('ROOK_SECRET_') or not key.replace('_','').isalnum() or not isinstance(value,str) or '\0' in value:raise RuntimeError('invalid named-secret environment')
        env[key]=value
    record['status']='running';record['proxy']=process(os.getpid());record['execution']=dict(started=False,quiet=False,code=None);save(record)
    child=None;quiet=False;started=time.monotonic();maximum=limits['output_bytes']//24
    out=bytearray();err=bytearray();received=0;truncated=False;last_pulse=0
    try:
        if interrupted[0]:raise RuntimeError('native launch cancelled before spawn')
        child=subprocess.Popen(['/bin/sh','-c',command],cwd=base+('/'+cwd if cwd else ''),env=env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
        record['execution']['started']=True;record['child']=process(child.pid);save(record)
        streams=selectors.DefaultSelector()
        for pipe,target in ((child.stdout,out),(child.stderr,err)):
            os.set_blocking(pipe.fileno(),False);streams.register(pipe,selectors.EVENT_READ,target)
        try:
            while streams.get_map() or child.poll() is None:
                if child.poll() is not None:cleanup()
                if interrupted[0] or time.monotonic()-started>request.get('timeout_secs',120):interrupted[0]=True;break
                for key,_ in streams.select(0.1):
                    chunk=os.read(key.fileobj.fileno(),4096)
                    if not chunk:streams.unregister(key.fileobj);continue
                    received+=len(chunk);target=key.data;take=min(len(chunk),max(0,maximum-len(target)));target.extend(chunk[:take]);truncated=truncated or take<len(chunk)
                if time.monotonic()-last_pulse>=0.5:
                    try:print(json.dumps(dict(progress=dict(call=control['call'],output_bytes=received))),flush=True)
                    except (BrokenPipeError,OSError):interrupted[0]=True;break
                    last_pulse=time.monotonic()
        finally:streams.close()
    finally:
        quiet=cleanup()
        if child is not None:
            try:child.wait(timeout=5)
            except subprocess.TimeoutExpired:quiet=False
            child.stdout.close();child.stderr.close()
        stdout=out.decode('utf-8',errors='replace');stderr=err.decode('utf-8',errors='replace')
        values=[value for value in request.get('environment',{}).values() if value]
        if values and truncated:stdout=stderr='[output withheld: named secrets were used and capture was truncated]'
        else:
            for value in values:stdout=stdout.replace(value,'[redacted]');stderr=stderr.replace(value,'[redacted]')
        record['execution'].update(quiet=quiet,code=None if interrupted[0] or child is None else child.returncode,interrupted=interrupted[0],stdout=stdout,stderr=stderr,truncated=truncated,output_bytes=received)
        save(record)
        if quiet:
            record['after']=snapshot_root(root);record['after_sha256']=snapshot_hash(record['after']);record['status']='applied'
        else:record['status']='running'
        save(record)
    return record

def hash_bytes(data):return hashlib.sha256(data).hexdigest() if data is not None else None

def source(root,path):
    parent,name=path.rsplit('/',1) if '/' in path else ('',path)
    fd=directory(root,parent)
    try:
        try:info=os.stat(name,dir_fd=fd,follow_symlinks=False)
        except FileNotFoundError:return None,None
        if not stat.S_ISREG(info.st_mode) or info.st_mode&0o7000:raise RuntimeError('native writes require an ordinary regular file')
        data=read(root,path)
        return data,dict(mode=stat.S_IMODE(info.st_mode),uid=info.st_uid,gid=info.st_gid)
    finally:os.close(fd)

def replace_source(root,path,data,attrs):
    parent,name=path.rsplit('/',1) if '/' in path else ('',path)
    fd=directory(root,parent);temporary='.rook-native-'+control['call']
    try:
        if data is None:os.unlink(name,dir_fd=fd)
        else:
            out=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=fd)
            try:
                with os.fdopen(out,'wb',closefd=False) as stream:stream.write(data);stream.flush();os.fsync(out)
                owner=os.fstat(out)
                if (owner.st_uid,owner.st_gid)!=(attrs['uid'],attrs['gid']):os.fchown(out,attrs['uid'],attrs['gid'])
                os.fchmod(out,attrs['mode'])
            finally:os.close(out)
            os.replace(temporary,name,src_dir_fd=fd,dst_dir_fd=fd)
        os.fsync(fd)
    finally:
        try:os.unlink(temporary,dir_fd=fd)
        except FileNotFoundError:pass
        os.close(fd)

def checkpoint_meta(record,current=None):
    result={key:record[key] for key in ('call','owner','identity','path','kind','status','before_sha256','after_sha256','arguments_sha256')}
    if 'execution' in record:result['execution']=record['execution']
    if record.get('kind')=='run':result['excluded_entry_names']=limits['exclude']
    if current is not None:result['current_sha256']=current[0];result['matches_before']=current[0]==record['before_sha256'];result['matches_after']=current[0]==record['after_sha256']
    return result

def checkpoint(root,args):
    global control
    control=request.get('control',{})
    alphabet='0123456789ABCDEFGHJKMNPQRSTVWXYZ'
    if not isinstance(control,dict) or not all(isinstance(control.get(k),str) and len(control[k])==26 and all(c in alphabet for c in control[k]) for k in ('owner','call')) or not isinstance(control.get('identity'),str) or len(control['identity'])!=64 or any(c not in '0123456789abcdef' for c in control['identity']):raise RuntimeError('checkpoint needs an exact client-owned session and receipt')
    # State stays outside the selected source tree, and is opened without links.
    top='/tmp/.rook-native-'+str(os.getuid())
    try:os.mkdir(top,0o700)
    except FileExistsError:pass
    fd=os.open(top,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    lock=None;owner=None
    try:
        info=os.fstat(fd)
        if info.st_uid!=os.getuid() or stat.S_IMODE(info.st_mode)!=0o700:raise RuntimeError('native checkpoint directory has changed ownership or permissions')
        physical=hashlib.sha256((str(os.fstat(root).st_dev)+':'+str(os.fstat(root).st_ino)).encode()).hexdigest()
        for part in (physical,control['identity'],control['owner']):
            try:os.mkdir(part,0o700,dir_fd=fd)
            except FileExistsError:pass
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd);os.close(fd);fd=child
        owner=fd;fd=None
        # One lock for the physical root, across separate frontend/session owners.
        lockroot=os.open(top+'/'+physical,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        try:lock=os.open('lock',os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600,dir_fd=lockroot)
        finally:os.close(lockroot)
        deadline=time.monotonic()+10
        while True:
            try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);break
            except BlockingIOError:
                if time.monotonic()>deadline:raise RuntimeError('native workspace is busy; inspect its active owner')
                time.sleep(0.02)
        filename=control['call']+'.json';maximum=limits['total_bytes']*3+limits['max_entries']*2560+16384
        def save(record,create=False):
            encoded=json.dumps(record,ensure_ascii=True,separators=(',',':')).encode()
            if len(encoded)>maximum:raise RuntimeError('checkpoint byte budget exceeded before workspace mutation')
            temporary='.'+filename+'.'+str(os.getpid())
            out=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=owner)
            try:
                with os.fdopen(out,'wb',closefd=False) as stream:stream.write(encoded);stream.flush();os.fsync(out)
                if create:
                    os.link(temporary,filename,src_dir_fd=owner,dst_dir_fd=owner,follow_symlinks=False);os.unlink(temporary,dir_fd=owner)
                else:os.replace(temporary,filename,src_dir_fd=owner,dst_dir_fd=owner)
                os.fsync(owner)
            finally:
                os.close(out)
                try:os.unlink(temporary,dir_fd=owner)
                except FileNotFoundError:pass
        if operation=='prepare':
            count=0
            with os.scandir(owner) as entries:
                for entry in entries:
                    count+=1
                    if count>=limits['max_calls']:raise RuntimeError('native checkpoint count reached max_calls; retain and review the receipts')
            if args.get('operation')=='run':
                if sys.platform!='linux' or not hasattr(os,'pidfd_open') or not hasattr(signal,'pidfd_send_signal'):raise RuntimeError('native execution requires Linux with pidfd and Python 3.9+')
                cwd=valid(args.get('path','.'),True);command=args.get('command')
                if not isinstance(command,str) or not 0<len(command.encode())<=128*1024:raise RuntimeError('invalid bounded native command')
                cwd_fd=directory(root,cwd);os.close(cwd_fd)
                before=snapshot_root(root)
                record=dict(version=1,base=base,identity=control['identity'],owner=control['owner'],call=control['call'],path=cwd or '.',kind='run',status='prepared',before=before,after=None,before_sha256=snapshot_hash(before),after_sha256=None,arguments_sha256=hashlib.sha256(json.dumps(args,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()).hexdigest())
                save(record,True);return checkpoint_meta(record)
            path=valid(args.get('path'));kind=args.get('operation');before,attrs=source(root,path)
            if kind=='edit':
                old=args.get('old');new=args.get('new')
                if before is None or not isinstance(old,str) or not old or not isinstance(new,str):raise RuntimeError('edit needs existing UTF-8 text and nonempty old/new strings')
                try:text=before.decode('utf-8')
                except UnicodeDecodeError:raise RuntimeError('edit source is not UTF-8')
                if text.count(old)!=1:raise RuntimeError('old text must match exactly once; read the current remote source')
                after=text.replace(old,new,1).encode()
            elif kind=='write':
                text=args.get('content')
                if not isinstance(text,str):raise RuntimeError('write needs text content')
                after=text.encode()
            else:raise RuntimeError('unsupported prepared mutation')
            if len(after)>limits['file_bytes']:raise RuntimeError('new native file exceeds file_bytes')
            if before==after:raise RuntimeError('native mutation changes no bytes')
            attrs=attrs or dict(mode=0o644,uid=os.getuid(),gid=os.getgid())
            record=dict(version=1,base=base,identity=control['identity'],owner=control['owner'],call=control['call'],path=path,kind=kind,status='prepared',before=base64.b64encode(before).decode() if before is not None else None,after=base64.b64encode(after).decode(),attrs=attrs,before_sha256=hash_bytes(before),after_sha256=hash_bytes(after),arguments_sha256=hashlib.sha256(json.dumps(args,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()).hexdigest())
            save(record,True);return checkpoint_meta(record)
        child=os.open(filename,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=owner)
        try:
            info=os.fstat(child)
            if not stat.S_ISREG(info.st_mode) or info.st_size>maximum:raise RuntimeError('invalid checkpoint file or bound')
            with os.fdopen(child,'rb',closefd=False) as stream:encoded=stream.read(maximum+1)
            if len(encoded)>maximum:raise RuntimeError('checkpoint grew beyond its bound')
            record=json.loads(encoded)
        finally:os.close(child)
        if record.get('version')!=1 or record.get('base')!=base or any(record.get(k)!=control[k] for k in ('owner','identity','call')):raise RuntimeError('checkpoint identity changed')
        if record['kind']=='run':
            before=record['before'];after=record['after']
            if snapshot_hash(before)!=record['before_sha256'] or (after is not None and snapshot_hash(after)!=record['after_sha256']):raise RuntimeError('native run checkpoint changed')
            current=snapshot_root(root);digest=snapshot_hash(current)
            if operation=='checkpoint':return checkpoint_meta(record,(digest,None))
            if operation=='apply':
                if record['status']!='prepared' or record['arguments_sha256']!=hashlib.sha256(json.dumps(args,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()).hexdigest():raise RuntimeError('native command was already admitted; never replay it')
                if digest!=record['before_sha256']:raise RuntimeError('native workspace changed after preparation')
                run_owned(root,args,record,save);return checkpoint_meta(record)
            if operation=='undo':
                if record['status'] in ('running','applying') or (record.get('execution',{}).get('started') and not record['execution'].get('quiet')):raise RuntimeError('native command quiescence is unconfirmed; do not restore over a possible live owner')
                if digest==record['before_sha256']:
                    record['status']='undone';save(record);return checkpoint_meta(record)
                if after is None or digest!=record['after_sha256']:raise RuntimeError('native workspace changed after execution; undo refused')
                record['status']='undoing';save(record);restore_root(root,current,before);record['status']='undone';save(record);return checkpoint_meta(record)
            raise RuntimeError('unsupported native run checkpoint operation')
        path=valid(record['path']);before=base64.b64decode(record['before'],validate=True) if record['before'] is not None else None;after=base64.b64decode(record['after'],validate=True)
        if hash_bytes(before)!=record['before_sha256'] or hash_bytes(after)!=record['after_sha256']:raise RuntimeError('checkpoint content changed')
        current,attrs=source(root,path);digest=hash_bytes(current)
        if operation=='checkpoint':return checkpoint_meta(record,(digest,attrs))
        if operation=='apply':
            if record['status']!='prepared' or record['arguments_sha256']!=hashlib.sha256(json.dumps(args,sort_keys=True,ensure_ascii=False,separators=(',',':')).encode()).hexdigest():raise RuntimeError('mutation is not prepared or was already admitted; never replay it')
            if digest!=record['before_sha256'] or (current is not None and attrs!=record['attrs']):raise RuntimeError('remote source changed after preparation; mutation refused')
            record['status']='applying';save(record)
            replace_source(root,path,after,record['attrs']);record['status']='applied';save(record)
        elif operation=='undo':
            if digest==record['before_sha256'] and (current is None or attrs==record['attrs']):
                record['status']='undone';save(record);return checkpoint_meta(record)
            if record['status']=='prepared':raise RuntimeError('this receipt never admitted a mutation; inspect the current source')
            if digest!=record['after_sha256'] or attrs!=record['attrs']:raise RuntimeError('remote source changed after the write; undo refused')
            record['status']='undoing';save(record)
            replace_source(root,path,before,record['attrs']);record['status']='undone';save(record)
        else:raise RuntimeError('unsupported checkpoint operation')
        return checkpoint_meta(record)
    finally:
        if lock is not None:os.close(lock)
        if owner is not None:os.close(owner)
        if fd is not None:os.close(fd)

try:
    if not isinstance(limits,dict) or not 1024<=limits['file_bytes']<=8*1024*1024 or not 4096<=limits['output_bytes']<=1024*1024 or not 1<=limits['max_files']<=8192 or not 1<=limits['max_entries']<=32768 or not 1024<=limits['total_bytes']<=64*1024*1024:raise RuntimeError('invalid native workspace limits')
    root=root_open()
    try:
        if operation=='read':result=read_text(root,request['arguments'])
        elif operation=='list':result=listing(root,request['arguments'])
        elif operation=='search':result=search(root,request['arguments'])
        elif operation in ('prepare','apply','checkpoint','undo'):result=checkpoint(root,request['arguments'])
        else:raise RuntimeError('unsupported native operation')
    finally:os.close(root)
    reply=json.dumps(dict(result=result),ensure_ascii=True,separators=(',',':'))
    if len(reply.encode())>limits['output_bytes']:raise RuntimeError('native result exceeds output_bytes; select a smaller source')
    print(reply)
except (RuntimeError,OSError,ValueError,TypeError,KeyError) as error:
    message=str(error) if isinstance(error,RuntimeError) else ('remote filesystem operation failed (errno '+str(error.errno)+')' if isinstance(error,OSError) else 'invalid native protocol or checkpoint; preserve the owned state for inspection')
    print(json.dumps(dict(error=message)));sys.exit(1)
