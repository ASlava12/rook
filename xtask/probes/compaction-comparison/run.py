"""Owned protocol checks for compaction-ab; these are not summary-quality data."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True, help='built compaction-ab example')
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not __debug__:
    parser.error('Run without Python optimization; assertions are verification.')
binary = args.binary.resolve()
if not binary.is_file() or not args.output.parent.is_dir() or args.output.exists():
    parser.error('Use an existing example binary and a fresh report path with an existing parent.')

calls = []
lock = threading.Lock()
current = {'mode': 'incomplete'}
text = 'Required fact: owned fixture keeps the default unchanged.'


class Service(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(5)

    def log_message(self, *unused):
        pass

    def reply(self, body, kind='application/json'):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Content-Type', kind)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.reply({'data': [{'id': 'fixture'}]})

    def do_POST(self):
        size = int(self.headers.get('Content-Length', 0))
        assert 0 < size <= 1024 * 1024
        request = json.loads(self.rfile.read(size))
        with lock:
            mode = current['mode']
            calls.append({'mode': mode, 'stream': request.get('stream'), 'model': request.get('model'),
                          'budget': request.get('max_tokens', request.get('max_completion_tokens'))})
            assert len(calls) <= 24
        delta = {'reasoning_content': text} if mode == 'reasoning' else {'content': '' if mode == 'empty' else text}
        chunk = {'model': 'fixture', 'choices': [{'index': 0, 'delta': delta,
                  'finish_reason': None if mode == 'incomplete' else 'length' if mode == 'limited' else 'stop'}]}
        if mode != 'unreported':
            chunk['usage'] = {'prompt_tokens': 20, 'completion_tokens': 7}
        wire = 'data: ' + json.dumps(chunk) + '\n\n'
        if mode != 'incomplete':
            wire += 'data: [DONE]\n\n'
        self.reply(wire.encode(), 'text/event-stream')


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with tempfile.TemporaryDirectory(prefix='rook-compaction-comparison-') as directory:
        root = Path(directory)
        home = root / 'home'
        home.mkdir()
        (home / 'config.toml').write_text(f'''[agent]
model='fixture'
compaction_model='fixture'
compaction_timeout_secs=5
[fallbacks]
main=[]
compaction=[]
[models.fixture]
api='openai'
model='fixture'
url='http://127.0.0.1:{server.server_port}/v1'
context_window=65536
''', encoding='utf-8')
        entries = [{'kind': 'user' if n % 2 == 0 else 'assistant',
                    'body': f'Owned task entry {n}: ' + ('Preserve the default and read saved source facts. ' * 12)}
                   for n in range(32)]
        fixture = root / 'fixture.json'
        fixture_data = {'name': 'protocol-fixture-not-quality-data', 'entries': entries,
                        'checks': ['default unchanged']}
        fixture.write_text(json.dumps([fixture_data]), encoding='utf-8')
        env = dict(os.environ, ROOK_HOME=str(home), ROOK_CONFIG_DIR=str(home), ROOK_LOG='error')
        cases = {}
        for mode in ['incomplete', 'empty', 'unreported', 'confirmed', 'reasoning', 'limited']:
            with lock:
                current['mode'] = mode
                before = len(calls)
            child = subprocess.run([str(binary), str(fixture)], env=env, capture_output=True,
                                   encoding='utf-8', timeout=45,
                                   creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            records = [json.loads(line) for line in child.stdout.splitlines() if line.strip()]
            with lock:
                requests = len(calls) - before
            cases[mode] = {'exit': child.returncode, 'requests': requests, 'records': records,
                           'stderr': child.stderr[-2000:]}

        # A valid first fixture must not spend requests before a malformed
        # later fixture is rejected. Validate the complete input batch first.
        invalid = dict(fixture_data, checks=['too many'] * 65)
        fixture.write_text(json.dumps([fixture_data, invalid]), encoding='utf-8')
        with lock:
            before = len(calls)
            current['mode'] = 'confirmed'
        invalid_child = subprocess.run([str(binary), str(fixture)], env=env, capture_output=True,
                                       encoding='utf-8', timeout=45,
                                       creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        admission = {'exit': invalid_child.returncode, 'requests': len(calls) - before,
                     'stdout': invalid_child.stdout, 'stderr': invalid_child.stderr[-2000:]}

        def stopped(case):
            return (case['exit'] != 0 and case['requests'] == 1 and len(case['records']) == 1
                    and not case['records'][0]['available']
                    and 'summary comparison is incomplete' in case['stderr'])

        def paired(case):
            records = case['records']
            return (case['exit'] == 0 and case['requests'] == 4 and len(records) == 4
                    and [record['output_budget'] for record in records] == [2048, 4096, 4096, 2048]
                    and [record['candidate'] for record in records] == [False, True, True, False]
                    and all(record['available'] and record.get('compaction_outcome') == 'summary'
                            and record.get('model_receipt', {}).get('complete') is True
                            and record.get('model_receipt', {}).get('usage_reported') is True
                            and record['checks'] == [{'term': 'default unchanged', 'preserved': True}]
                            for record in records))

        rejected = [cases[mode]['records'][0] for mode in ['incomplete', 'empty', 'unreported']
                    if cases[mode]['records']]
        checks = {
            'incomplete_response_stops_comparison': stopped(cases['incomplete']),
            'empty_summary_stops_comparison': stopped(cases['empty']),
            'unreported_usage_stops_comparison': stopped(cases['unreported']),
            'ineligible_arms_have_no_quality_scores': len(rejected) == 3 and all(record['checks'] == [] for record in rejected),
            'diagnostics_and_usage_are_retained': (len(rejected) == 3 and all(record['usage'] and record.get('model_receipt') for record in rejected)
                and rejected[0].get('compaction_outcome') == 'source_fallback'
                and rejected[0].get('model_receipt', {}).get('complete') is False
                and rejected[1].get('compaction_outcome') == 'source_fallback'
                and rejected[2].get('compaction_outcome') == 'summary'
                and rejected[2].get('model_receipt', {}).get('usage_reported') is False),
            'confirmed_summary_retains_abba': paired(cases['confirmed']),
            'confirmed_reasoning_summary_retains_abba': paired(cases['reasoning']),
            'admitted_output_budget_recorded': all(record.get('admitted_output_tokens') == record['output_budget']
                                                  for case in cases.values() for record in case['records']),
            'termination_and_truncation_reported': (len(rejected) == 3 and paired(cases['limited'])
                and all(record.get('stop_reason') == 'max_tokens' and record.get('truncated') is True for record in cases['limited']['records'])
                and all(record.get('stop_reason') == 'end_turn' and record.get('truncated') is False
                        for mode in ['confirmed', 'reasoning'] for record in cases[mode]['records'])
                and rejected[0].get('truncated') is None),
            'reasoning_text_bytes_distinct': (all(record.get('text_bytes') == 0 and record.get('reasoning_text_bytes', 0) > 0
                                              for record in cases['reasoning']['records'])
                and all(record.get('text_bytes', 0) > 0 and record.get('reasoning_text_bytes') == 0 for record in cases['confirmed']['records'])),
            'all_fixtures_admitted_before_any_request': (admission['exit'] != 0 and admission['requests'] == 0
                and not admission['stdout'].strip() and 'fixture entry/check ceiling exceeded' in admission['stderr']),
            'exact_owned_streaming_requests': len(calls) == 15 and all(call['stream'] is True and call['model'] == 'fixture' for call in calls),
        }
        report = {'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                  'checks': checks, 'cases': cases, 'calls': calls, 'admission': admission, 'quality_experiment': False}
        with args.output.open('x', encoding='utf-8') as sink:
            json.dump(report, sink, indent=2)
        print(json.dumps({'binary_sha256': report['binary_sha256'], 'checks': checks}), flush=True)
        assert all(checks.values()), checks
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=2)
