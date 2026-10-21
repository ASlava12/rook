"""Verify Rook release archives against an immutable source and build identity."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tarfile
import tempfile
import zipfile

if not __debug__:
    raise RuntimeError('Release verification requires Python without optimization (-O).')

def safe_member_name(name):
    assert isinstance(name, str) and name, ('empty member', name)
    assert not any(char in name for char in '\\<>:"|?*'), ('nonportable path', name)
    assert not any(ord(char) < 32 for char in name), ('control in path', name)
    path = PurePosixPath(name)
    parts = name.removesuffix('/').split('/')
    assert not path.is_absolute() and all(part not in ('', '.', '..') for part in parts), name
    for part in parts:
        assert not part.endswith((' ', '.')), ('Windows path alias', name)
        # Win32 also reserves the ISO-8859-1 superscript port digits.
        assert not re.fullmatch(r'(?i)(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(?:\..*)?', part), name
    return path

def pe_imports(image):
    pe = int.from_bytes(image[60:64], 'little')
    optional = pe + 24
    assert int.from_bytes(image[optional:optional + 2], 'little') == 0x20b
    sections = optional + int.from_bytes(image[pe + 20:pe + 22], 'little')
    count = int.from_bytes(image[pe + 6:pe + 8], 'little')
    def position(rva):
        for index in range(count):
            section = sections + index * 40
            size = int.from_bytes(image[section + 8:section + 12], 'little')
            virtual = int.from_bytes(image[section + 12:section + 16], 'little')
            raw_size = int.from_bytes(image[section + 16:section + 20], 'little')
            raw = int.from_bytes(image[section + 20:section + 24], 'little')
            if virtual <= rva < virtual + max(size, raw_size):
                offset = raw + rva - virtual
                assert 0 <= offset < len(image)
                return offset
        raise AssertionError(('unmapped import RVA', rva))
    imports = []
    directory = int.from_bytes(image[optional + 120:optional + 124], 'little')
    if directory:
        cursor = position(directory)
        for _ in range(256):
            descriptor = image[cursor:cursor + 20]
            assert len(descriptor) == 20
            if not any(descriptor):
                return imports
            start = position(int.from_bytes(descriptor[12:16], 'little'))
            end = image.index(b'\0', start, start + 256)
            imports.append(image[start:end].decode('ascii'))
            cursor += 20
        raise AssertionError('oversized import directory')
    return imports

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('artifacts', type=Path)
    p.add_argument('--skills', type=Path, required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--host', action='store_true', help='extract and run both Windows binaries; Windows only')
    p.add_argument('--target', action='append')
    p.add_argument('--source-commit', required=True)
    p.add_argument('--recipe-commit', required=True)
    p.add_argument('--run-url', required=True)
    a = p.parse_args()
    if a.host and os.name != 'nt':
        p.error('--host requires Windows; omit it for portable archive/linkage checks')
    targets = ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
               'x86_64-pc-windows-msvc', 'aarch64-apple-darwin', 'x86_64-apple-darwin']
    if a.target:
        assert set(a.target) <= set(targets), a.target
        assert len(a.target) == len(set(a.target)), 'duplicate target selection'
        targets = a.target
    archives = sorted([*a.artifacts.rglob('*.zip'), *a.artifacts.rglob('*.tar.gz')])
    assert len(archives) == len(targets), [str(f) for f in archives]
    skills = {f.relative_to(a.skills).as_posix() for f in a.skills.rglob('SKILL.md')}
    skill_files = {f.relative_to(a.skills).as_posix(): f for f in a.skills.rglob('*') if f.is_file()}
    assert skills, 'the tagged skill source contains no SKILL.md files'
    assert all(not f.is_symlink() for f in a.skills.rglob('*')), 'linked source skill resources'
    checksums = {}
    manifests = [*a.artifacts.rglob('*.sha256'), *a.artifacts.rglob('SHA256SUMS')]
    provided = {path for path in a.artifacts.rglob('*') if path.is_file()}
    assert provided == set(archives) | set(manifests), ('unexpected artifact files', provided - set(archives) - set(manifests))
    for manifest in manifests:
        for line in manifest.read_text(encoding='utf-8').splitlines():
            m = re.fullmatch(r'([a-fA-F0-9]{64})\s+\*?([^/\\]+)', line)
            assert m, (manifest, line)
            name, value = m[2], m[1].lower()
            assert name not in checksums or checksums[name] == value, name
            checksums[name] = value
    if not a.target:
        assert set(checksums) == {archive.name for archive in archives}, 'checksum manifest coverage'
    verified = []
    host_dir = None
    for archive in archives:
        target = next(t for t in targets if t in archive.name)
        expected_name = f'rook-{a.version}-{target}' + ('.zip' if 'windows' in target else '.tar.gz')
        assert archive.name == expected_name, ('archive version/name', archive.name, expected_name)
        assert target not in [item['target'] for item in verified], target
        with archive.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        assert checksums.get(archive.name) == digest, ('checksum', archive.name)
        if archive.suffix == '.zip':
            container = zipfile.ZipFile(archive)
            entries = container.infolist()
            member_names = [entry.filename for entry in entries]
            assert all(stat.S_IFMT(entry.external_attr >> 16) in (0, stat.S_IFREG, stat.S_IFDIR) for entry in entries), ('non-regular ZIP member', archive)
            assert sum(entry.file_size for entry in entries) <= 1024 * 1024 * 1024, archive
            files = {entry.filename: entry for entry in entries if not entry.is_dir()}
            read = lambda name: container.read(name)
        else:
            container = tarfile.open(archive, 'r:gz')
            entries = container.getmembers()
            member_names = [entry.name for entry in entries]
            assert all(entry.isfile() or entry.isdir() for entry in entries), archive
            assert sum(entry.size for entry in entries) <= 1024 * 1024 * 1024, archive
            files = {entry.name: entry for entry in entries if entry.isfile()}
            read = lambda name: container.extractfile(name).read()
        with container:
            assert len(member_names) == len(set(member_names)), ('duplicate members', archive)
            assert len(member_names) == len({name.rstrip('/').casefold() for name in member_names}), ('case/path aliases', archive)
            for name in member_names:
                safe_member_name(name)
            names = set(files)
            roots = {PurePosixPath(name).parts[0] for name in names}
            assert len(roots) == 1, roots
            root = roots.pop()
            expected_root = archive.name[:-4] if archive.suffix == '.zip' else archive.name[:-7]
            assert root == expected_root, (root, expected_root)
            suffix = '.exe' if target == 'x86_64-pc-windows-msvc' else ''
            required = {'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', f'bin/rook{suffix}', f'bin/rookd{suffix}'}
            relative = {name[len(root) + 1:] for name in names}
            assert required <= relative, (target, required - relative)
            if a.source_commit or a.recipe_commit or a.run_url:
                assert 'BUILD-INFO.txt' in relative, (target, 'missing build provenance')
                pairs = [line.split('=', 1) for line in read(f'{root}/BUILD-INFO.txt').decode('utf-8').splitlines()]
                assert all(len(pair) == 2 for pair in pairs)
                info = dict(pairs)
                assert len(info) == len(pairs), (target, 'duplicate build provenance keys')
                expected = {'version': a.version, 'target': target,
                            'rustflags': '-C target-feature=+crt-static' if 'windows' in target else ''}
                for argument, key in [(a.source_commit, 'source_commit'), (a.recipe_commit, 'recipe_commit'), (a.run_url, 'run_url')]:
                    if argument:
                        expected[key] = argument
                for key, value in expected.items():
                    assert info.get(key) == value, (target, key, info.get(key), value)
            packaged_skills = {name.removeprefix('share/rook/skills/') for name in relative
                               if name.startswith('share/rook/skills/') and name.endswith('/SKILL.md')}
            assert packaged_skills == skills, (target, skills - packaged_skills, packaged_skills - skills)
            packaged_skill_files = {name.removeprefix('share/rook/skills/') for name in relative if name.startswith('share/rook/skills/')}
            assert packaged_skill_files == set(skill_files), (target, 'skill resources', set(skill_files) - packaged_skill_files, packaged_skill_files - set(skill_files))
            for skill, source in skill_files.items():
                actual, expected_bytes = read(f'{root}/share/rook/skills/{skill}'), source.read_bytes()
                if source.suffix == '.md':
                    actual, expected_bytes = actual.replace(b'\r\n', b'\n'), expected_bytes.replace(b'\r\n', b'\n')
                assert actual == expected_bytes, (target, skill)
            allowed = required | {'BUILD-INFO.txt'} | {f'share/rook/skills/{name}' for name in skill_files}
            assert relative <= allowed, (target, 'unexpected package files', relative - allowed)
            assert a.version in read(f'{root}/README.md').decode('utf-8'), target
            for binary in ['rook', 'rookd']:
                image = read(f'{root}/bin/{binary}{suffix}')
                assert len(image) > 1_000_000, (target, binary)
                if 'linux' in target:
                    assert image[:6] == b'\x7fELF\x02\x01', (target, binary, 'ELF64 little-endian')
                    assert int.from_bytes(image[18:20], 'little') == (183 if target.startswith('aarch64') else 62), (target, binary, 'ELF machine')
                    phoff = int.from_bytes(image[32:40], 'little')
                    phsize = int.from_bytes(image[54:56], 'little')
                    phcount = int.from_bytes(image[56:58], 'little')
                    assert phsize == 56 and phoff + phsize * phcount <= len(image), (target, binary, 'ELF program headers')
                    for index in range(phcount):
                        header = image[phoff + index * phsize:phoff + (index + 1) * phsize]
                        kind = int.from_bytes(header[:4], 'little')
                        assert kind != 3, (target, binary, 'unexpected ELF interpreter')
                        if kind == 2:
                            start = int.from_bytes(header[8:16], 'little')
                            size = int.from_bytes(header[32:40], 'little')
                            assert start + size <= len(image) and size % 16 == 0, (target, binary, 'ELF dynamic table')
                            for offset in range(start, start + size, 16):
                                tag = int.from_bytes(image[offset:offset + 8], 'little', signed=True)
                                if tag == 0:
                                    break
                                assert tag != 1, (target, binary, 'unexpected shared-library dependency')
                elif 'apple' in target:
                    assert image[:4] == b'\xcf\xfa\xed\xfe', (target, binary, 'Mach-O64')
                    assert int.from_bytes(image[4:8], 'little') == (0x0100000c if target.startswith('aarch64') else 0x01000007), (target, binary, 'Mach-O CPU')
                else:
                    offset = int.from_bytes(image[60:64], 'little')
                    assert image[:2] == b'MZ' and image[offset:offset + 4] == b'PE\x00\x00', (target, binary, 'PE')
                    assert int.from_bytes(image[offset + 4:offset + 6], 'little') == 0x8664, (target, binary, 'PE machine')
                    dependencies = pe_imports(image)
                    assert not any(re.match(r'(?i)(vcruntime\d|msvcp\d|msvcr\d|ucrtbase\.|api-ms-win-crt-)', name) for name in dependencies), (target, binary, 'dynamic CRT', dependencies)
            if a.host and target == 'x86_64-pc-windows-msvc':
                host_dir = Path(tempfile.mkdtemp(prefix='rook-package-'))
                for name in names:
                    destination = host_dir.joinpath(*PurePosixPath(name).parts)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.write_bytes(read(name))
                for binary in ['rook', 'rookd']:
                    result = subprocess.run([str(host_dir / root / 'bin' / (binary + '.exe')), '--version'],
                                            text=True, capture_output=True, check=True, timeout=30)
                    assert result.stdout.strip() == f'{binary} {a.version}', result.stdout
                host_dir /= root
            verified.append({'target': target, 'archive': archive.name, 'sha256': digest, 'skills': len(skills), 'skill_files': len(skill_files)})
    print(json.dumps({'verified': verified, 'host_package': str(host_dir) if host_dir else None}))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
