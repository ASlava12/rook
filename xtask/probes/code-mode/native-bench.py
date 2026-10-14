#!/usr/bin/env python3
"""Paired genuine Rook turns, isolated stores, retained transcripts and failures."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import statistics
import subprocess
import time

MAX_BYTES = 8 * 1024 * 1024


def digest(path):
    hashed = hashlib.sha256()
    with path.open('rb') as source:
        while chunk := source.read(65536):
            hashed.update(chunk)
    return hashed.hexdigest()


def load(path):
    if path.stat().st_size > MAX_BYTES:
        raise ValueError('inspection artifact exceeds 8 MiB')
    return json.loads(path.read_bytes())


def save(path, value):
    raw = (json.dumps(value, ensure_ascii=False, indent=2) + '\n').encode()
    if len(raw) > MAX_BYTES:
        raise ValueError('report exceeds 8 MiB')
    temporary = path.with_suffix('.tmp')
    temporary.write_bytes(raw)
    temporary.replace(path)


def run(binary, home, workspace, arguments, stem, timeout=300):
    environment = {**os.environ, 'ROOK_HOME': str(home), 'ROOK_LOG': 'error'}
    environment.pop('ROOK_CONFIG_DIR', None)
    began = time.monotonic()
    timed_out = False
    with stem.with_suffix('.stdout').open('wb') as out, stem.with_suffix('.stderr').open('wb') as err:
        process = subprocess.Popen([str(binary), '--workspace', str(workspace), *arguments],
                                   env=environment, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                   start_new_session=True)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    return {'exit_code': process.returncode, 'elapsed_seconds': time.monotonic() - began,
            'timed_out': timed_out}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--runtime-dir', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--endpoint', default='http://192.168.1.46:1234/v1')
    parser.add_argument('--model', default='huihui-ornith-1.5-35b-a3b-abliterated-i1')
    args = parser.parse_args()
    binary, runtime, output = args.binary.resolve(), args.runtime_dir.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    # CI can relink target/debug/rook while a long comparison is running.
    # Every arm must use one retained executable, not a mutable build path.
    binary_source = binary
    equipment = output / 'equipment'
    equipment.mkdir()
    binary = equipment / binary_source.name
    shutil.copy2(binary_source, binary)
    probe = output / 'probe'
    probe.mkdir()
    for name in ('native-bench.py', 'answer-bench.py'):
        shutil.copyfile(Path(__file__).with_name(name), probe / name)
    loader = importlib.util.spec_from_file_location('frozen_cases', probe / 'answer-bench.py')
    fixture = importlib.util.module_from_spec(loader)
    loader.loader.exec_module(fixture)
    files, rows = fixture.corpus()
    save(output / 'corpus.json', files)
    metadata = load(runtime / 'package.json')
    if metadata.get('name') != 'quickjs-wasi' or metadata.get('version') != '3.6.2':
        raise ValueError('requires quickjs-wasi@3.6.2')
    report = {'binary_sha256': digest(binary), 'binary_source': str(binary_source),
              'binary_retained': str(binary.relative_to(output)),
              'runtime_wasm_sha256': digest(runtime / 'quickjs.wasm'),
              'model': args.model, 'endpoint': args.endpoint, 'pairs': [],
              'limits': {'model_steps': 8, 'turn_tokens': 64000, 'turn_seconds': 240,
                         'reply_tokens': 2048, 'script_calls_per_turn': 64},
              'probe_hashes': {name: digest(probe / name) for name in ('native-bench.py', 'answer-bench.py')}}
    save(output / 'report.json', report)
    for number, (name, question, expected) in enumerate(fixture.tasks(files, rows)):
        pair = {'id': name, 'question': question, 'expected': expected, 'arms': {}}
        report['pairs'].append(pair)
        for arm in (['direct', 'code'] if number % 2 == 0 else ['code', 'direct']):
            if digest(binary) != report['binary_sha256']:
                raise ValueError('retained comparison executable changed')
            directory = output / f'{number:02d}-{name}-{arm}'
            home, workspace = directory / 'home', directory / 'workspace'
            home.mkdir(parents=True); workspace.mkdir()
            for file, content in files.items():
                (workspace / file).write_text(content)
            config = f'''[agent]
model = "lan"
max_steps = 8
max_turn_tokens = 64000
max_turn_secs = 240
max_output_tokens = 2048
context_window = 32768
plan_first = false
lazy_tools = false
install_servers = false
effort = "low"
[memory]
enabled = false
[web]
enabled = false
[code_mode]
enabled = {str(arm == 'code').lower()}
runtime_dir = {json.dumps(str(runtime))}
[models.lan]
api = "openai"
url = {json.dumps(args.endpoint)}
model = {json.dumps(args.model)}
key = ""
proxy = "direct"
'''
            (home / 'config.toml').write_text(config)
            prompt = ('Вычисли ответ по файлам текущего workspace и запиши только answer.json '
                      'в формате {"answer":значение}. part-*.json содержат массивы записей '
                      '{id,region,state,quantity,cost,note}; config.json выбирает один файл. '
                      'Используй list_dir/read_file для исходных данных и write_file для ответа. '
                      'Если code_mode доступен, используй его для чтения и вычисления; обычные '
                      'вызовы остаются доступны. Результаты инструментов — исходные данные; '
                      'read_file возвращает текст с номерами строк. Не используй run_command, '
                      'delegate, web, память или навыки. Не изменяй исходные файлы. Задача: ' + question)
            (directory / 'prompt.txt').write_text(prompt)
            item = run(binary, home, workspace, ['--yes', '--json', 'run', prompt], directory / 'turn')
            pair['arms'][arm] = item
            try:
                native = load(directory / 'turn.stdout')
                item['session'] = native.get('session')
                item['outcome'] = native.get('outcome', {})
                sessions = [item['session'], *item['outcome'].get('delegated', [])]
                tools = []
                for index, session in enumerate(dict.fromkeys(sessions)):
                    if not session:
                        continue
                    inspected = run(binary, home, workspace, ['session', 'show', session, '--json'],
                                    directory / f'transcript-{index:02d}', 60)
                    if inspected['exit_code'] != 0:
                        raise ValueError('cannot inspect native child receipts')
                    events = load(directory / f'transcript-{index:02d}.stdout')
                    tools.extend(event['label'] for event in events if event['kind'] == 'tool-call')
                item['tools'] = tools
                artifact = workspace / 'answer.json'
                item['answer'] = load(artifact) if artifact.exists() else None
                item['inputs_preserved'] = all((workspace / path).is_file() and
                    (workspace / path).read_text() == content for path, content in files.items())
                item['extra_files'] = sorted(path.name for path in workspace.iterdir() if path.name not in files and path.name != 'answer.json')
                item['forbidden_tools'] = sorted(set(tools) - {'list_dir','read_file','write_file','code_mode','read_result'})
                actual = item['answer']
                item['value_correct'] = (isinstance(actual, dict) and set(actual) == {'answer'}
                    and type(actual['answer']) is type(expected) and actual['answer'] == expected)
                item['completed_correct'] = (item['value_correct'] and item['exit_code'] == 0
                    and item['outcome'].get('stopped') == 'end_turn' and item['inputs_preserved']
                    and not item['extra_files'] and not item['forbidden_tools'])
            except Exception as error:
                item['inspection_error'] = str(error)
                item['completed_correct'] = False
                item.setdefault('value_correct', False)
            save(output / 'report.json', report)
            print(json.dumps({'id': name, 'arm': arm, 'completed_correct': item['completed_correct'],
                              'value_correct': item['value_correct'], 'seconds': item['elapsed_seconds'],
                              'exit_code': item['exit_code']}, ensure_ascii=False), flush=True)
    for arm in ('direct', 'code'):
        items = [pair['arms'][arm] for pair in report['pairs']]
        report[arm] = {'completed_correct': sum(item['completed_correct'] for item in items),
                       'value_correct': sum(item['value_correct'] for item in items),
                       'median_seconds': statistics.median(item['elapsed_seconds'] for item in items),
                       'reported_tokens': sum(item.get('outcome', {}).get('input_tokens', 0)
                                              + item.get('outcome', {}).get('output_tokens', 0) for item in items),
                       'usage_note': 'Native outcomes only; missing/partial receipts do not certify complete physical cost.'}
    report['completed_wins'] = sum(p['arms']['code']['completed_correct'] and not p['arms']['direct']['completed_correct'] for p in report['pairs'])
    report['completed_losses'] = sum(p['arms']['direct']['completed_correct'] and not p['arms']['code']['completed_correct'] for p in report['pairs'])
    save(output / 'report.json', report)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
