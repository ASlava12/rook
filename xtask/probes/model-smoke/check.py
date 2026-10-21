"""Offline preparation regressions, including HTTP 200 with a streamed error."""
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import tempfile
import unittest

import verify

MODEL = 'qwen2.5-coder:3b'
VERIFIER = Path(__file__).with_name('verify.py')
NO_WINDOW = getattr(subprocess, 'CREATE_NO_WINDOW', 0)
git = shutil.which('git')
git_bash = Path(git).resolve().parents[1] / 'bin/bash.exe' if git else None
BASH = str(git_bash) if os.name == 'nt' and git_bash and git_bash.is_file() else shutil.which('bash')


class PreparationChecks(unittest.TestCase):
    def parse(self, body):
        verify.pull(io.BytesIO(body), report=lambda text: None)

    def test_streamed_error_rejects_even_a_previous_success(self):
        for body in [b'{"error":"pull model manifest: 503"}\n',
                     b'{"status":"success"}\n{"error":"upstream failed"}\n']:
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, 'returned an error'):
                self.parse(body)

    def test_missing_or_retracted_terminal_success_is_not_a_complete_pull(self):
        for body in [b'', b'\n', b'{"status":"pulling manifest"}\n',
                     b'{"status":"success"}\n{"status":"pulling manifest"}\n']:
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, 'final success'):
                self.parse(body)

    def test_real_progress_then_final_success_is_accepted(self):
        self.parse(b'{"status":"pulling manifest"}\n{"status":"pulling sha256:abc","completed":100}\n'
                   b'{"status":"verifying sha256 digest"}\n{"status":"success"}\n\n')

    def test_malformed_and_oversized_progress_is_refused(self):
        for body in [b'not json\n', b'null\n', b'[]\n', b'{}\n', b'x' * (verify.MAX_LINE + 1)]:
            with self.subTest(size=len(body)), self.assertRaises(ValueError):
                self.parse(body)
        with self.assertRaisesRegex(ValueError, 'record limit'):
            self.parse(b'{"status":"pulling manifest"}\n' * (verify.MAX_RECORDS + 1))

    def test_inventory_requires_the_actual_requested_model(self):
        for response in [{'models': None}, {'models': []}, {'models': [{'name': 'another:tag'}]},
                         {'error': 'not ready'}, []]:
            with self.subTest(response=response), self.assertRaises(ValueError):
                verify.inventory(io.BytesIO(json.dumps(response).encode()), MODEL)
        for key in ['name', 'model']:
            verify.inventory(io.BytesIO(json.dumps({'models': [{key: MODEL}]}).encode()), MODEL)
        with self.assertRaisesRegex(ValueError, 'byte limit'):
            verify.inventory(io.BytesIO(b'x' * (verify.MAX_INVENTORY + 1)), MODEL)

    def test_optimized_python_still_rejects_error_and_accepts_success(self):
        for body, code in [(b'{"error":"503"}\n', 1), (b'{"status":"success"}\n', 0)]:
            child = subprocess.run([sys.executable, '-O', str(VERIFIER), 'pull'], input=body,
                                   capture_output=True, timeout=5, creationflags=NO_WINDOW)
            self.assertEqual(child.returncode, code, child.stderr)

    def test_actual_http_200_is_not_mistaken_for_a_successful_pull(self):
        curl = shutil.which('curl')
        self.assertIsNotNone(curl, 'curl is required by the workflow under test')

        class Service(http.server.BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def do_GET(self):
                body = (b'{"error":"pull model manifest: 503"}\n' if self.path == '/failed'
                        else b'{"status":"pulling manifest"}\n{"status":"success"}\n')
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
        thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': .01}, daemon=True)
        thread.start()
        try:
            for route, code in [('failed', 1), ('success', 0)]:
                downloaded = subprocess.run([curl, '--fail', '--silent', '--show-error', '--max-time', '3',
                                             f'http://127.0.0.1:{server.server_port}/{route}'],
                                            capture_output=True, timeout=5, creationflags=NO_WINDOW)
                self.assertEqual(downloaded.returncode, 0, downloaded.stderr)
                checked = subprocess.run([sys.executable, str(VERIFIER), 'pull'], input=downloaded.stdout,
                                         capture_output=True, timeout=5, creationflags=NO_WINDOW)
                self.assertEqual(checked.returncode, code, checked.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def prepare(self, fail_pulls=0, missing_model=False, broken_transfer=False, unavailable=False):
        self.assertIsNotNone(BASH, 'Bash is required by the workflow under test')
        counts = {'pull': 0, 'tags': 0}
        requests = []

        class Service(http.server.BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def reply(self, body, status=200, extra_length=0):
                self.send_response(status)
                self.send_header('Content-Length', str(len(body) + extra_length))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                counts['tags'] += 1
                if unavailable:
                    self.reply(b'{}', status=503)
                else:
                    models = [] if missing_model else [{'name': MODEL}]
                    self.reply(json.dumps({'models': models}).encode())

            def do_POST(self):
                counts['pull'] += 1
                size = int(self.headers.get('Content-Length', 0))
                requests.append(json.loads(self.rfile.read(min(size, 8192))))
                if counts['pull'] <= fail_pulls:
                    self.reply(b'{"status":"pulling manifest"}\n{"error":"manifest upstream 503"}\n')
                else:
                    self.reply(b'{"status":"success"}\n', extra_length=100 if broken_transfer else 0)

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
        thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': .01}, daemon=True)
        thread.start()
        try:
            # Run the exact shipped script; shorten only sleep in this child
            # shell so failure/retry admission is tested without waiting 10s.
            script = VERIFIER.with_name('prepare.sh').resolve().as_posix()
            env = dict(os.environ, ROOK_SMOKE_PYTHON=Path(sys.executable).as_posix())
            child = subprocess.run([BASH, '--noprofile', '--norc', '-c',
                                    'sleep() { :; }; source "$1" "$2" "$3"', '--', script,
                                    f'http://127.0.0.1:{server.server_port}', MODEL],
                                   env=env, capture_output=True, timeout=30, creationflags=NO_WINDOW)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
        self.assertTrue(all(request == {'model': MODEL} for request in requests), requests)
        return child, counts

    def test_real_preparation_retries_a_stream_error_then_confirms_inventory(self):
        child, counts = self.prepare(fail_pulls=1)
        self.assertEqual(child.returncode, 0, child.stderr)
        self.assertEqual(counts, {'pull': 2, 'tags': 2})
        self.assertIn(b'requested model is available', child.stdout)

    def test_real_preparation_stops_after_three_failed_pulls(self):
        child, counts = self.prepare(fail_pulls=3)
        self.assertEqual(child.returncode, 1, child.stderr)
        self.assertEqual(counts, {'pull': 3, 'tags': 1})
        self.assertIn(b'no smoke scenarios ran', child.stderr)

    def test_real_preparation_refuses_an_absent_model_after_successful_pull(self):
        child, counts = self.prepare(missing_model=True)
        self.assertEqual(child.returncode, 1, child.stderr)
        self.assertEqual(counts, {'pull': 3, 'tags': 4})

    def test_success_record_does_not_override_curl_transfer_failure(self):
        child, counts = self.prepare(broken_transfer=True)
        self.assertEqual(child.returncode, 1, child.stderr)
        self.assertEqual(counts, {'pull': 3, 'tags': 1})

    def test_unavailable_server_exhausts_readiness_without_any_pull(self):
        child, counts = self.prepare(unavailable=True)
        self.assertEqual(child.returncode, 1, child.stderr)
        self.assertEqual(counts, {'pull': 0, 'tags': 30})
        self.assertIn(b'did not become ready', child.stderr)


if __name__ == '__main__':
    unittest.main()
