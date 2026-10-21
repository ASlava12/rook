"""Negative archive fixtures: recomputed checksums must not bless damaged contents."""
import hashlib
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile

from verify import safe_member_name

VERIFIER = Path(__file__).with_name('verify.py')
TARGET = 'x86_64-pc-windows-msvc'
ROOT = f'rook-1.2.3-{TARGET}'
RESOURCE = f'{ROOT}/share/rook/skills/demo/variants/windows.md'


class PackageChecks(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='rook-package-negative-')
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.artifacts = root / 'artifacts'
        self.artifacts.mkdir()
        self.skills = root / 'skills'
        (self.skills / 'demo/variants').mkdir(parents=True)
        (self.skills / 'demo/SKILL.md').write_text('---\nname: demo\ndescription: Demo\n---\nBody.\n')
        (self.skills / 'demo/variants/windows.md').write_text('Exact resource.\n')
        info = {'version': '1.2.3', 'target': TARGET, 'source_commit': 'a' * 40,
                'recipe_commit': 'b' * 40, 'run_url': 'https://example.invalid/run/1',
                'rustflags': '-C target-feature=+crt-static'}
        self.files = {f'{ROOT}/{name}': b'fixture' for name in ['LICENSE-MIT', 'LICENSE-APACHE', 'bin/rook.exe', 'bin/rookd.exe']}
        self.files[f'{ROOT}/README.md'] = b'Rook 1.2.3'
        self.files[f'{ROOT}/BUILD-INFO.txt'] = ''.join(f'{key}={value}\n' for key, value in info.items()).encode()
        for path in self.skills.rglob('*'):
            if path.is_file():
                self.files[f'{ROOT}/share/rook/skills/{path.relative_to(self.skills).as_posix()}'] = path.read_bytes()

    def rejected(self, reason, extra=None, extra_file=False, optimized=False):
        archive = self.artifacts / f'{ROOT}.zip'
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(archive, 'w') as output:
                for name, data in self.files.items():
                    output.writestr(name, data)
                if extra is not None:
                    output.writestr(*extra)
        # A failure below cannot be credited to the checksum guard.
        with archive.open('rb') as stream:
            checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
        (self.artifacts / 'SHA256SUMS').write_text(f'{checksum}  {archive.name}\n')
        if extra_file:
            (self.artifacts / 'unreviewed.txt').write_text('not a release asset')
        command = [sys.executable, *(['-O'] if optimized else []), str(VERIFIER), str(self.artifacts),
                   '--skills', str(self.skills), '--version', '1.2.3', '--target', TARGET,
                   '--source-commit', 'a' * 40, '--recipe-commit', 'b' * 40,
                   '--run-url', 'https://example.invalid/run/1']
        result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(reason, result.stderr)
        self.assertNotIn('"verified"', result.stdout)

    def test_missing_variant_is_rejected_before_binary_inspection(self):
        del self.files[RESOURCE]
        self.rejected('skill resources')

    def test_changed_variant_is_rejected_before_binary_inspection(self):
        self.files[RESOURCE] = b'different resource'
        self.rejected('demo/variants/windows.md')

    def test_duplicate_members_are_rejected(self):
        self.rejected('duplicate members', (RESOURCE, b'second version'))

    def test_case_aliases_are_rejected(self):
        self.rejected('case/path aliases', (RESOURCE.replace('windows.md', 'WINDOWS.md'), b'alias'))

    def test_zip_symlinks_are_rejected(self):
        link = zipfile.ZipInfo(f'{ROOT}/link')
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        self.rejected('non-regular ZIP member', (link, b'../outside'))

    def test_extra_artifact_files_are_not_uploaded_silently(self):
        self.rejected('unexpected artifact files', extra_file=True)

    def test_wrong_build_source_is_rejected(self):
        name = f'{ROOT}/BUILD-INFO.txt'
        self.files[name] = self.files[name].replace(b'a' * 40, b'c' * 40)
        self.rejected('source_commit')

    def test_optimization_cannot_disable_verification(self):
        self.rejected('without optimization', optimized=True)

    def test_portable_paths_reject_traversal_drives_controls_and_aliases(self):
        for name in ['../escape', '/absolute', 'C:/escape', 'root/a/../escape', 'root\\escape',
                     'root//file', 'root/./file', 'root/file.', 'root/file ', 'root/NUL',
                     'root/COM1.txt', 'root/a\nb', 'root/a?b', 'root/a*b', 'root/a|b', 'root/a<b']:
            with self.subTest(name=name), self.assertRaises(AssertionError):
                safe_member_name(name)
        self.assertEqual(str(safe_member_name('rook-1.2.3/share/skills/界.md')), 'rook-1.2.3/share/skills/界.md')

    def test_windows_superscript_device_aliases_are_rejected(self):
        for family in ['COM', 'LPT']:
            for digit in '¹²³':
                for suffix in ['', '.md']:
                    name = f'root/{family}{digit}{suffix}'
                    with self.subTest(name=name), self.assertRaises(AssertionError):
                        safe_member_name(name)


if __name__ == '__main__':
    unittest.main()
