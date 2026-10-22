"""Owned normal CLI self-update with truncated bodies and strict Range controls."""
import argparse
import hashlib
import http.server
import io
import json
import os
import re
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--tmp-root', type=Path)
args = parser.parse_args()
assert __debug__
output = args.output.resolve()
assert not output.exists() and output.parent.is_dir()
bins = args.bin_dir.resolve()
suffix = '.exe' if os.name == 'nt' else ''
root = Path(tempfile.mkdtemp(prefix='rook-asset-recovery-', dir=args.tmp_root)).resolve()
(root / 'logs').mkdir()

def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

binaries = {name: {'path': str(bins / (name + suffix)), 'sha256': sha(bins / (name + suffix))}
            for name in ['rook', 'rookd']}
report = {'status': 'incomplete', 'owned_directory': str(root), 'binaries': binaries,
          'script_sha256': sha(Path(__file__).resolve()), 'cases': []}

def command(arguments, name, env, cwd, timeout=90):
    child = subprocess.Popen(arguments, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    captures = {'stdout': bytearray(), 'stderr': bytearray()}
    overflow = threading.Event()
    def drain(label, stream):
        while chunk := stream.read1(4096):
            room = 65536 - len(captures[label])
            captures[label].extend(chunk[:room])
            if len(chunk) > room:
                overflow.set()
                if child.poll() is None:
                    child.kill()
    readers = [threading.Thread(target=drain, args=(name, stream), daemon=True)
               for name, stream in [('stdout', child.stdout), ('stderr', child.stderr)]]
    for reader in readers:
        reader.start()
    try:
        code = child.wait(timeout=timeout)
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        for reader in readers:
            reader.join(timeout=10)
        assert not any(reader.is_alive() for reader in readers)
        child.stdout.close()
        child.stderr.close()
    for label, data in captures.items():
        (root / 'logs' / (name + '.' + label)).write_bytes(data)
    assert not overflow.is_set(), 'owned CLI exceeded64KiB capture per stream'
    return code, bytes(captures['stdout']), bytes(captures['stderr'])

def environment(home, api):
    env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home),
               ROOK_RELEASE_API=api, ROOK_UPDATE_REPO='owned/rook', ROOK_LOG='error')
    env.pop('ROOK_BUILTIN_SKILLS', None)
    return env

def workspace_digest(path):
    return {p.relative_to(path).as_posix(): {'kind': 'file', 'sha256': sha(p)}
            if p.is_file() else {'kind': 'directory'} for p in path.rglob('*')}

wire = []
errors = []
lock = threading.Lock()
asset_bytes = None
asset_name = None
asset_sha = None
version = None
prefix_bytes = 137531
modes = ['resume', 'ignored-range', 'retry-cap', 'bad-range', 'bad-digest']

class Server(http.server.BaseHTTPRequestHandler):
    def log_message(self, *unused):
        pass
    def send_bytes(self, data, status=200, extra=None, length=None):
        self.send_response(status)
        self.send_header('Content-Type', 'application/octet-stream')
        self.send_header('Content-Length', str(len(data) if length is None else length))
        self.send_header('Connection', 'close')
        for key, value in (extra or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()
        self.close_connection = True
    def do_GET(self):
        try:
            self.connection.settimeout(20)
            parts = self.path.split('/')
            assert len(self.path) <= 512 and len(parts) >= 3
            mode = parts[1]
            assert mode in modes + ['warmup']
            metadata = self.path == '/' + mode + '/repos/owned/rook/releases/latest'
            asset = self.path == '/' + mode + '/asset' and mode in modes
            assert metadata or asset
            with lock:
                assert len(wire) < 32
                previous = sum(row['mode'] == mode and row['kind'] == 'asset' for row in wire)
                wire.append({'mode': mode, 'kind': 'metadata' if metadata else 'asset',
                             'range': self.headers.get('Range'),
                             'accept_encoding': self.headers.get('Accept-Encoding')})
            if metadata:
                digest = ('0' * 64 if asset_sha != '0' * 64 else '1' * 64) if mode == 'bad-digest' else asset_sha
                assets = [] if mode == 'warmup' else [{'name': asset_name, 'size': len(asset_bytes),
                    'digest': 'sha256:' + digest,
                    'browser_download_url': f'http://127.0.0.1:{server.server_port}/{mode}/asset'}]
                self.send_bytes(json.dumps({'tag_name': 'v' + version, 'assets': assets}).encode())
                return
            assert asset_bytes is not None and previous < 3
            requested = self.headers.get('Range')
            if previous == 0:
                assert requested is None
                count = len(asset_bytes) if mode == 'bad-digest' else prefix_bytes
                self.send_bytes(asset_bytes[:count], length=len(asset_bytes))
            else:
                assert requested and requested.startswith('bytes=') and requested.endswith('-')
                offset = int(requested[6:-1])
                assert 0 < offset < len(asset_bytes)
                if mode == 'ignored-range':
                    self.send_bytes(asset_bytes)
                else:
                    start = offset - 1 if mode == 'bad-range' else offset
                    content_range = f'bytes {start}-{len(asset_bytes)-1}/{len(asset_bytes)}'
                    end = min(len(asset_bytes), offset + prefix_bytes) if mode in ['retry-cap', 'bad-range'] else len(asset_bytes)
                    tail = asset_bytes[offset:end]
                    self.send_bytes(tail, 206, {'Content-Range': content_range}, len(asset_bytes) - offset)
        except (BrokenPipeError, ConnectionResetError):
            # A header/digest refusal can close its owned connection while the server writes.
            pass
        except BaseException as error:
            with lock:
                if len(errors) < 8:
                    errors.append(type(error).__name__ + ': ' + str(error)[:500])
            self.close_connection = True
    def do_POST(self):
        with lock:
            if len(errors) < 8:
                errors.append('Unexpected model/POST request')
        self.send_error(500)

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
server.daemon_threads = True
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    version_home = root / 'version-home'
    version_home.mkdir()
    version_env = environment(version_home, f'http://127.0.0.1:{server.server_port}/warmup')
    for name, binary in binaries.items():
        code, stdout, stderr = command([binary['path'], '--version'], 'version-' + name, version_env, root, 30)
        assert code == 0, stderr[-2000:]
        binary['version'] = stdout.decode().strip()
    version = binaries['rook']['version'].removeprefix('rook ')
    assert re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?', version)
    assert binaries['rookd']['version'] == 'rookd ' + version
    warm = root / 'warmup'
    warm.mkdir()
    env = environment(warm, f'http://127.0.0.1:{server.server_port}/warmup')
    code, stdout, stderr = command([binaries['rook']['path'], '--json', 'update', '--check'], 'warmup', env, warm)
    assert code == 0, stderr[-2000:]
    target = json.loads(stdout)['target']
    assert target in ['x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc',
                      'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
                      'x86_64-apple-darwin', 'aarch64-apple-darwin']
    top = f'rook-{version}-{target}'
    extension = '.zip' if 'windows' in target else '.tar.gz'
    asset_name = top + extension
    new_skill = b'---\nname: owned-recovery\n---\nOwned verified replacement.\n'
    old_skill = b'---\nname: owned-old\n---\nOwned original.\n'
    payload_dir = root / 'payload-bin'
    payload_dir.mkdir()
    payloads = {}
    for name, binary in binaries.items():
        path = payload_dir / (name + suffix)
        shutil.copy2(binary['path'], path)
        if sys.platform.startswith('linux'):
            # Hosted source builds contain large DWARF sections. Keep the original
            # updater unmodified and remove only debug data from its replacement copy.
            strip = shutil.which('strip')
            assert strip, 'Linux source payloads require the host strip tool'
            code, stdout, stderr = command([strip, '--strip-debug', str(path)], 'strip-' + name, version_env, root)
            assert code == 0, stderr[-2000:]
        code, stdout, stderr = command([str(path), '--version'], 'payload-version-' + name, version_env, root, 30)
        assert code == 0 and stdout.decode().strip() == binary['version'], stderr[-2000:]
        payloads[name] = {'path': str(path), 'sha256': sha(path), 'version': binary['version'],
                          'debug_sections_stripped': sys.platform.startswith('linux')}
    report['payload_binaries'] = payloads
    stream = io.BytesIO()
    if extension == '.zip':
        with zipfile.ZipFile(stream, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            for name, binary in payloads.items():
                archive.write(binary['path'], top + '/bin/' + name + suffix)
            archive.writestr(top + '/share/rook/skills/owned-recovery/SKILL.md', new_skill)
    else:
        with tarfile.open(fileobj=stream, mode='w:gz') as archive:
            for name, binary in payloads.items():
                info = archive.gettarinfo(binary['path'], arcname=top + '/bin/' + name + suffix)
                assert info.isfile() and not info.issym()
                info.mode = stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH
                with Path(binary['path']).open('rb') as source_file:
                    archive.addfile(info, source_file)
            info = tarfile.TarInfo(top + '/share/rook/skills/owned-recovery/SKILL.md')
            info.mode, info.size = 0o644, len(new_skill)
            archive.addfile(info, io.BytesIO(new_skill))
    asset_bytes = stream.getvalue()
    assert prefix_bytes < len(asset_bytes) <= 128 * 1024 * 1024, ('fixture archive size', len(asset_bytes))
    asset_sha = hashlib.sha256(asset_bytes).hexdigest()
    archive_path = root / asset_name
    assert archive_path.parent == root and not archive_path.exists()
    archive_path.write_bytes(asset_bytes)
    assert sha(archive_path) == asset_sha
    report.update(version=version, target=target, archive=str(archive_path), asset_size=len(asset_bytes), asset_sha256=asset_sha)
    for mode in modes:
        print(json.dumps({'case': mode}), flush=True)
        case = root / mode
        home = case / 'home'
        workspace = case / 'workspace'
        install = case / 'installation'
        home.mkdir(parents=True)
        workspace.mkdir()
        (install / 'bin').mkdir(parents=True)
        skills = install / 'share/rook/skills'
        (skills / 'owned-old').mkdir(parents=True)
        (skills / 'owned-old/SKILL.md').write_bytes(old_skill)
        (workspace / 'sentinel.txt').write_bytes(b'Owned workspace unchanged')
        before = workspace_digest(workspace)
        for name, binary in binaries.items():
            shutil.copy2(binary['path'], install / 'bin' / (name + suffix))
        before_install = workspace_digest(install)
        env = environment(home, f'http://127.0.0.1:{server.server_port}/{mode}')
        code, stdout, stderr = command([str(install / 'bin' / ('rook' + suffix)), '--json', 'update', '--force'], mode, env, workspace)
        after_install = workspace_digest(install)
        expected_files = {f'bin/{name}{suffix}': binary['sha256'] for name, binary in payloads.items()}
        expected_files.update({f'bin/{name}{suffix}.previous': binary['sha256'] for name, binary in binaries.items()})
        expected_files.update({'share/rook/skills/owned-recovery/SKILL.md': hashlib.sha256(new_skill).hexdigest(),
                              'share/rook/skills.previous/owned-old/SKILL.md': hashlib.sha256(old_skill).hexdigest()})
        expected_tree = {}
        for relative, digest in expected_files.items():
            expected_tree[relative] = {'kind': 'file', 'sha256': digest}
            for parent in Path(relative).parents:
                if str(parent) != '.':
                    expected_tree[parent.as_posix()] = {'kind': 'directory'}
        expected_success = mode in ['resume', 'ignored-range']
        after_binary = all(sha(install / 'bin' / (name + suffix)) == binary['sha256'] for name, binary in payloads.items())
        original_bytes = all(sha(install / 'bin' / (name + suffix)) == binary['sha256'] for name, binary in binaries.items())
        previous_binary = all((install / 'bin' / (name + suffix + '.previous')).is_file()
            and sha(install / 'bin' / (name + suffix + '.previous')) == binary['sha256'] for name, binary in binaries.items())
        new_skills = (skills / 'owned-recovery/SKILL.md').is_file() and (skills / 'owned-recovery/SKILL.md').read_bytes() == new_skill
        previous_skills = (install / 'share/rook/skills.previous/owned-old/SKILL.md').is_file() and (install / 'share/rook/skills.previous/owned-old/SKILL.md').read_bytes() == old_skill
        unchanged_skills = (skills / 'owned-old/SKILL.md').is_file() and (skills / 'owned-old/SKILL.md').read_bytes() == old_skill and not (skills / 'owned-recovery').exists()
        no_backups = not any((install / 'bin').glob('*.previous')) and not (install / 'share/rook/skills.previous').exists()
        result = json.loads(stdout) if code == 0 else None
        observed = [row for row in wire if row['mode'] == mode and row['kind'] == 'asset']
        # Symbolized backtraces can fill the tail on hosted source builds.
        # Check the actual error line; the complete bounded capture stays in logs.
        error_line = stderr.decode('utf-8', errors='replace').split('\n', 1)[0][:2000]
        print(json.dumps({'case': mode, 'exit_code': code, 'asset_requests': len(observed),
                          'error_line': error_line}), flush=True)
        report['cases'].append({'mode': mode, 'exit_code': code, 'receipt': result,
            'expected_success': expected_success, 'asset_requests': observed,
            'installed_bytes_match': after_binary, 'previous_binaries_match': previous_binary,
            'new_skills_match': new_skills, 'previous_skills_match': previous_skills,
            'refusal_has_no_effects': after_install == before_install and original_bytes and unchanged_skills and no_backups,
            'exact_successful_tree': after_install == expected_tree,
            'installation_before': before_install, 'installation_after': after_install,
            'workspace_unchanged': workspace_digest(workspace) == before,
            'error_line': error_line,
            'stderr_tail': stderr.decode('utf-8', errors='replace')[-2000:]})
    cases = {row['mode']: row for row in report['cases']}
    def succeeded(name, count):
        row = cases[name]
        return row['exit_code'] == 0 and row['receipt']['to'] == version and len(row['asset_requests']) == count
    successes = [cases[name] for name in ['resume', 'ignored-range']]
    refusals = [cases[name] for name in ['retry-cap', 'bad-range', 'bad-digest']]
    headers = all(row['accept_encoding'] == 'identity' for row in wire if row['kind'] == 'asset')
    ranges = all(not row['asset_requests'][0]['range'] and all(
        request['range'] == f'bytes={prefix_bytes * index}-' for index, request in enumerate(row['asset_requests']) if index
    ) for row in report['cases'])
    report['checks'] = {
        'cli_resumes_truncated_asset': succeeded('resume', 2),
        'cli_restarts_when_range_ignored': succeeded('ignored-range', 2),
        'three_attempt_limit_enforced': cases['retry-cap']['exit_code'] != 0 and len(cases['retry-cap']['asset_requests']) == 3 and 'after 3 attempts' in cases['retry-cap']['error_line'],
        'mismatched_range_refused': cases['bad-range']['exit_code'] != 0 and len(cases['bad-range']['asset_requests']) == 2 and 'Content-Range' in cases['bad-range']['error_line'],
        'digest_refusal_is_terminal': cases['bad-digest']['exit_code'] != 0 and len(cases['bad-digest']['asset_requests']) == 1 and 'does not match the digest' in cases['bad-digest']['error_line'],
        'refusals_leave_installation_unchanged': all(row['refusal_has_no_effects'] for row in refusals),
        'successful_installs_have_exact_payloads': all(row['exit_code'] == 0 and row['installed_bytes_match'] and row['new_skills_match'] and row['exact_successful_tree'] for row in successes),
        'successful_installs_keep_previous_files': all(row['exit_code'] == 0 and row['previous_binaries_match'] and row['previous_skills_match'] for row in successes),
        'only_owned_metadata_and_asset_requests': not errors and len(wire) <= 17 and all(sum(row['mode'] == mode and row['kind'] == 'metadata' for row in wire) == 1 for mode in modes + ['warmup']),
        'range_and_identity_representation_preserved': headers and ranges,
        'workspace_unchanged': all(row['workspace_unchanged'] for row in report['cases']),
        'original_binary_pair_unchanged': all(sha(Path(binary['path'])) == binary['sha256'] for binary in binaries.values()),
    }
    assert len(report['checks']) == 12
    report['status'] = 'passed' if all(report['checks'].values()) else 'failed'
except BaseException as error:
    report['failure'] = type(error).__name__ + ': ' + str(error)[:2000]
    raise
finally:
    server.shutdown()
    server.server_close()
    report['owned_requests'], report['observer_errors'] = wire, errors
    with output.open('x', encoding='utf-8') as sink:
        json.dump(report, sink, indent=2)
    print(json.dumps({'status': report['status'], 'checks': report.get('checks'), 'output': str(output),
                      'failure': report.get('failure')}), flush=True)
assert report['status'] == 'passed', report.get('checks')
