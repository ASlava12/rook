# Sent as fixed code over an authenticated SSH channel; request data stays on stdin.
import base64, ctypes, fcntl, hashlib, json, os, signal, stat, struct, subprocess, sys, time
cfg = json.loads(sys.argv[1])
mode, base, identity, owner = cfg['action'], cfg['base'], cfg['identity'], cfg['owner']
limits = cfg['limits']
MAX_META = 8192
MAX_INPUT = 1024 * 1024
MAX_PROCESSES = 32768
root = os.path.join(base, '.rook-exec', identity, owner)

def valid(path):
    return isinstance(path, str) and 0 < len(path.encode()) <= 1024 and '\\' not in path and all(ord(c) >= 32 and ord(c) != 127 for c in path) and all(p not in ('', '.', '..') for p in path.split('/'))

def excluded(path):
    return any(part in limits['exclude'] for part in path.split('/'))

def open_dir(path, create=False):
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.split('/'):
            if not part:
                continue
            if part in ('.', '..'):
                raise RuntimeError('remote base path has a parent component')
            if create:
                try: os.mkdir(part, 0o700, dir_fd=fd)
                except FileExistsError: pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd); fd = child
        return fd
    except BaseException:
        os.close(fd); raise

def relative_dir(fd, path, create=False):
    current = os.dup(fd)
    try:
        for part in path.split('/') if path else []:
            if create:
                try: os.mkdir(part, 0o700, dir_fd=current)
                except FileExistsError: pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
            os.close(current); current = child
        return current
    except BaseException:
        os.close(current); raise

def small_read(fd, name):
    child = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
    try:
        info = os.fstat(child)
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_META: raise RuntimeError('invalid control metadata')
        data = os.read(child, MAX_META + 1)
        if len(data) > MAX_META: raise RuntimeError('control metadata grew past its bound')
        return json.loads(data)
    finally: os.close(child)

def small_write(fd, name, value):
    data = json.dumps(value, separators=(',', ':'), ensure_ascii=True).encode()
    if len(data) > MAX_META: raise RuntimeError('control metadata exceeds its bound')
    temp = '.' + name + '.' + str(os.getpid())
    child = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
    try:
        with os.fdopen(child, 'wb', closefd=False) as out: out.write(data); out.flush(); os.fsync(child)
        os.replace(temp, name, src_dir_fd=fd, dst_dir_fd=fd)
    finally:
        os.close(child)
        try: os.unlink(temp, dir_fd=fd)
        except FileNotFoundError: pass

def snapshot(fd):
    dirs, files, count, total = [], {}, 0, 0
    def visit(current, prefix):
        nonlocal count, total
        with os.scandir(current) as entries:
            for entry in entries:
                count += 1
                if count > limits['max_entries']: raise RuntimeError('remote max_entries exceeded')
                path = prefix + entry.name
                if not valid(path): raise RuntimeError('remote path is not bounded UTF-8')
                if excluded(path): continue
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    dirs.append(path)
                    child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
                    try: visit(child, path + '/')
                    finally: os.close(child)
                elif stat.S_ISREG(info.st_mode):
                    if len(files) >= limits['max_files']: raise RuntimeError('remote max_files exceeded')
                    child = os.open(entry.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=current)
                    try:
                        before = os.fstat(child)
                        if not stat.S_ISREG(before.st_mode) or before.st_size > limits['file_bytes'] or before.st_size > limits['total_bytes'] - total: raise RuntimeError('remote file_bytes/total_bytes exceeded')
                        with os.fdopen(child, 'rb', closefd=False) as source: data = source.read(before.st_size + 1)
                        after = os.fstat(child)
                        if len(data) != before.st_size or (before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns): raise RuntimeError('remote source changed during capture')
                    finally: os.close(child)
                    total += len(data)
                    files[path] = {'mode': stat.S_IMODE(before.st_mode) & 0o777, 'data': base64.b64encode(data).decode('ascii')}
                else: raise RuntimeError('remote source contains a link or special entry; exclude it explicitly')
    visit(fd, '')
    return {'dirs': sorted(dirs), 'files': dict(sorted(files.items()))}

def source_hash(source):
    digest = hashlib.sha256()
    for path in source['dirs']:
        data = path.encode(); digest.update(b'd' + struct.pack('>I', len(data)) + data)
    for path, file in source['files'].items():
        data = path.encode(); digest.update(b'f' + struct.pack('>I', len(data)) + data + struct.pack('>I', file['mode']) + hashlib.sha256(base64.b64decode(file['data'], validate=True)).digest())
    return digest.hexdigest()

def emit(value):
    sys.stdout.write(json.dumps(value, separators=(',', ':'), ensure_ascii=True) + '\n')

def read_source():
    dirs, files, total, count, terminal = [], {}, 0, 0, None
    max_line = ((limits['file_bytes'] + 2) // 3) * 4 + MAX_META
    while True:
        line = sys.stdin.buffer.readline(max_line + 1)
        if not line: break
        if len(line) > max_line: raise RuntimeError('incoming frame exceeds its bound')
        value = json.loads(line)
        if terminal is not None: raise RuntimeError('input continued after source certificate')
        if value.get('type') == 'end': terminal = value['hash']; continue
        path = value.get('path'); count += 1
        if count > limits['max_entries'] or not valid(path) or excluded(path): raise RuntimeError('incoming paths exceed the declared contract')
        if value.get('type') == 'dir':
            if path in dirs or path in files: raise RuntimeError('duplicate incoming path')
            dirs.append(path)
        elif value.get('type') == 'file':
            text = value['data']
            if not isinstance(text, str) or len(text) > ((limits['file_bytes'] + 2) // 3) * 4 or len(files) >= limits['max_files']: raise RuntimeError('incoming file exceeds its bound')
            data = base64.b64decode(text, validate=True)
            if len(data) > limits['file_bytes'] or len(data) > limits['total_bytes'] - total: raise RuntimeError('incoming file bytes exceed their bound')
            total += len(data)
            if path in files or path in dirs or not isinstance(value['mode'], int) or not 0 <= value['mode'] <= 0o777: raise RuntimeError('invalid incoming file identity')
            files[path] = {'mode': value['mode'], 'data': text}
        else: raise RuntimeError('unsupported incoming frame')
    source = {'dirs': sorted(dirs), 'files': dict(sorted(files.items()))}
    if terminal != source_hash(source): raise RuntimeError('incoming source certificate mismatch')
    return source

def publish(fd, before, after):
    if source_hash(snapshot(fd)) != source_hash(before): raise RuntimeError('remote replica changed before publication')
    # A local file/directory replacement must remove its old shape first.
    for path in before['files']:
        if path in after['files']: continue
        parent, name = path.rsplit('/', 1) if '/' in path else ('', path)
        child = relative_dir(fd, parent)
        try: os.unlink(name, dir_fd=child)
        finally: os.close(child)
    for path in reversed(before['dirs']):
        if path in after['dirs']: continue
        parent, name = path.rsplit('/', 1) if '/' in path else ('', path)
        child = relative_dir(fd, parent)
        try: os.rmdir(name, dir_fd=child)
        finally: os.close(child)
    for path in after['dirs']:
        child = relative_dir(fd, path, True); os.close(child)
    for path, new in after['files'].items():
        if before['files'].get(path) == new: continue
        parent, name = path.rsplit('/', 1) if '/' in path else ('', path)
        child = relative_dir(fd, parent, True)
        try:
            temp = '.rook-sync-' + cfg['token']
            out = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=child)
            try:
                data = base64.b64decode(new['data'], validate=True)
                with os.fdopen(out, 'wb', closefd=False) as stream: stream.write(data); stream.flush(); os.fsync(out)
                os.fchmod(out, new['mode']); os.replace(temp, name, src_dir_fd=child, dst_dir_fd=child)
            finally:
                os.close(out)
                try: os.unlink(temp, dir_fd=child)
                except FileNotFoundError: pass
        finally: os.close(child)

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

def stop(fd, state):
    if state.get('terminal') and state.get('stopped'): return True
    if state.get('status') == 'prepared' and state.get('stopped'):
        state.update(status='interrupted', terminal=True, stopped=True, code=None)
        small_write(fd, 'run.json', state)
        return True
    proxy = state.get('proxy')
    if not proxy:
        if state.get('status') == 'prepared':
            state.update(status='interrupted', terminal=True, stopped=True, code=None)
            small_write(fd, 'run.json', state)
            return True
        return False
    current = process(proxy['pid'])
    if not current or current['birth'] != proxy['birth']: return False
    signal_owned(current, signal.SIGTERM)
    deadline = time.monotonic() + 7
    while time.monotonic() < deadline:
        latest = small_read(fd, 'run.json')
        if latest.get('token') == state['token'] and latest.get('terminal') and latest.get('stopped'): return True
        time.sleep(0.02)
    return False

def remove_owned_tree(fd):
    entries = []
    def gather(current, prefix):
        with os.scandir(current) as stream:
            for entry in stream:
                path = prefix + entry.name
                if len(entries) >= limits['max_entries']: raise RuntimeError('owned cleanup exceeds max_entries; inspect the retained replica before manual cleanup')
                kind = entry.stat(follow_symlinks=False)
                entries.append((path, stat.S_ISDIR(kind.st_mode)))
                if stat.S_ISDIR(kind.st_mode):
                    child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
                    try: gather(child, path + '/')
                    finally: os.close(child)
    gather(fd, '')
    for path, directory in reversed(entries):
        parent, name = path.rsplit('/', 1) if '/' in path else ('', path)
        child = relative_dir(fd, parent)
        try:
            if directory: os.rmdir(name, dir_fd=child)
            else: os.unlink(name, dir_fd=child)
        finally: os.close(child)


try:
    if sys.platform != 'linux' or not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'): raise RuntimeError('SSH worker needs Linux, Python 3.9+ and pidfd support')
    if not base.startswith('/') or '..' in base.split('/') or not all(c in '0123456789abcdef' for c in identity) or len(identity) != 64 or not all(c.isalnum() for c in owner): raise RuntimeError('invalid owned resource identity')
    try: fd = open_dir(root, mode == 'init')
    except FileNotFoundError:
        if mode not in ('inspect', 'release'): raise
        emit({'missing': True, 'identity': identity, 'owner': owner, 'workspace': root + '/workspace'})
        sys.exit(0)
    try:
        if mode == 'init':
            try: marker = small_read(fd, 'owner.json')
            except FileNotFoundError:
                marker = {'identity': identity, 'owner': owner, 'source': None}; small_write(fd, 'owner.json', marker)
            if marker['identity'] != identity or marker['owner'] != owner: raise RuntimeError('SSH resource identity changed')
            child = relative_dir(fd, 'workspace', True); os.close(child)
            emit({'ready': True, 'workspace': root + '/workspace', 'identity': identity, 'owner': owner})
        else:
            marker = small_read(fd, 'owner.json')
            if marker['identity'] != identity or marker['owner'] != owner: raise RuntimeError('SSH resource identity mismatch')
            if mode == 'upload':
                transfer = {'token': cfg['token'], 'status': 'uploading', 'terminal': False, 'stopped': False, 'command_started': False, 'proxy': process(os.getpid())}
                def cancel_upload(_number, _frame): raise InterruptedError('SSH transfer cancelled')
                signal.signal(signal.SIGTERM, cancel_upload); signal.signal(signal.SIGHUP, cancel_upload)
                small_write(fd, 'run.json', transfer)
                try:
                    after = read_source(); workspace = relative_dir(fd, 'workspace')
                    try:
                        before = snapshot(workspace); expected = cfg.get('previous')
                        if (expected is not None and source_hash(before) != expected) or (expected is None and (before['dirs'] or before['files'])): raise RuntimeError('remote replica has unreviewed changes')
                        publish(workspace, before, after)
                        transfer.update(status='prepared', terminal=False, stopped=True, source=source_hash(after))
                        small_write(fd, 'run.json', transfer)
                        emit({'prepared': True, 'source': source_hash(after)})
                    finally: os.close(workspace)
                except BaseException:
                    transfer.update(status='upload_interrupted', terminal=True, stopped=True)
                    small_write(fd, 'run.json', transfer)
                    raise
            elif mode == 'run':
                raw = sys.stdin.buffer.readline(MAX_INPUT + 1)
                if len(raw) > MAX_INPUT or sys.stdin.buffer.read(1): raise RuntimeError('command request exceeds its bound')
                request = json.loads(raw)
                if not isinstance(request.get('command'), str) or len(request['command'].encode()) > 128 * 1024: raise RuntimeError('command exceeds its bound')
                cwd = request['cwd']
                if cwd and not valid(cwd): raise RuntimeError('invalid command cwd')
                cwd_fd = open_dir(root + '/workspace' + ('/' + cwd if cwd else '')); os.close(cwd_fd)
                env = dict(os.environ)
                for key, value in request.get('env', {}).items():
                    if not key.startswith('ROOK_SECRET_') or not key.replace('_', '').isalnum() or not isinstance(value, str) or '\0' in value: raise RuntimeError('invalid per-call secret environment')
                    env[key] = value
                if ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) != 0: raise RuntimeError('could not own adopted descendants')
                interrupted = [False]
                def interrupt(_signum, _frame): interrupted[0] = True
                signal.signal(signal.SIGTERM, interrupt); signal.signal(signal.SIGHUP, interrupt); signal.signal(signal.SIGINT, interrupt)
                lock = os.open('lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=fd)
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX)
                    previous = small_read(fd, 'run.json')
                    if previous.get('token') != cfg['token'] or previous.get('status') != 'prepared': raise RuntimeError('command was not prepared or was already started')
                    if interrupted[0]: raise InterruptedError('SSH launch cancelled before command spawn')
                    proxy = process(os.getpid())
                    small_write(fd, 'run.json', {'token': cfg['token'], 'status': 'launching', 'proxy': proxy, 'terminal': False, 'stopped': False, 'command_started': False})
                    child = subprocess.Popen(['/bin/sh', '-c', request['command']], cwd=root + '/workspace' + ('/' + cwd if cwd else ''), env=env, stdin=subprocess.DEVNULL, start_new_session=True)
                    state = {'token': cfg['token'], 'status': 'running', 'proxy': proxy, 'terminal': False, 'stopped': False, 'command_started': True}
                    small_write(fd, 'run.json', state); fcntl.flock(lock, fcntl.LOCK_UN)
                    while child.poll() is None and not interrupted[0]: time.sleep(0.02)
                    code = child.poll()
                    stopped = cleanup()
                    state.update(status='interrupted' if interrupted[0] else 'finished', terminal=True, stopped=stopped, code=None if interrupted[0] else (128 - code if code is not None and code < 0 else code))
                    small_write(fd, 'run.json', state)
                    sys.exit(125 if interrupted[0] or not stopped else (128 - code if code is not None and code < 0 else code or 0))
                finally: os.close(lock)
            elif mode in ('inspect', 'stop', 'dump', 'release'):
                try: state = small_read(fd, 'run.json')
                except FileNotFoundError: state = {'status': 'ready', 'terminal': True, 'stopped': True}
                if cfg.get('token') and state.get('token') != cfg['token']: raise RuntimeError('remote call identity changed')
                if mode in ('stop', 'release'):
                    lock = os.open('lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=fd)
                    lock_deadline = time.monotonic() + 5
                    while True:
                        try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB); break
                        except BlockingIOError:
                            if time.monotonic() >= lock_deadline: os.close(lock); raise RuntimeError('SSH launch lock did not become available')
                            time.sleep(0.01)
                    try:
                        try: state = small_read(fd, 'run.json')
                        except FileNotFoundError: pass
                        stopped = stop(fd, state)
                    finally: os.close(lock)
                    if not stopped: raise RuntimeError('owned remote process cleanup could not be certified')
                    if mode == 'release':
                        # Keep source/control material available for explicit recovery; release
                        # stops execution and marks this immutable resource, never another tree.
                        remove_owned_tree(fd)
                        parent = open_dir(os.path.dirname(root))
                        try: os.rmdir(owner, dir_fd=parent)
                        finally: os.close(parent)
                        emit({'stopped': True, 'released': True, 'identity': identity, 'owner': owner})
                    else: emit({'stopped': True, 'state': small_read(fd, 'run.json') if state.get('token') else state})
                elif mode == 'inspect':
                    if state.get('proxy'):
                        current = process(state['proxy']['pid']); state['running'] = bool(current and current['birth'] == state['proxy']['birth'] and not state.get('terminal'))
                    emit({'state': state, 'marker': marker, 'workspace': root + '/workspace'})
                else:
                    if not state.get('stopped') or not state.get('terminal'): raise RuntimeError('remote command is not certified quiescent')
                    workspace = relative_dir(fd, 'workspace')
                    try: source = snapshot(workspace)
                    finally: os.close(workspace)
                    marker['source'] = source_hash(source); small_write(fd, 'owner.json', marker)
                    emit({'type': 'meta', 'state': state, 'identity': identity, 'owner': owner})
                    for path in source['dirs']: emit({'type': 'dir', 'path': path})
                    for path, file in source['files'].items(): emit({'type': 'file', 'path': path, **file})
                    emit({'type': 'end', 'hash': source_hash(source)})
            else: raise RuntimeError('unsupported SSH worker action')
    finally: os.close(fd)
except Exception as error:
    sys.stderr.write('SSH worker refused: ' + str(error) + '\n'); sys.exit(125)
