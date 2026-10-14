#!/usr/bin/env python3
"""Paired actual Rook tasks with local embeddings and synthetic, isolated memory.

Each arm must write an exact grounded fact and ID to answer.json through an
ordinary tool call. Cold stores, fixed settings, alternating arm order, no
retries, and all failures retained. No installed daemon is contacted or restarted.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

HERE = Path(__file__).resolve().parent
MAX_BYTES = 8 * 1024 * 1024


def bounded_json(path):
    if path.stat().st_size > MAX_BYTES:
        raise ValueError('artifact exceeds 8 MiB')
    return json.loads(path.read_bytes())


def save(path, value):
    raw = (json.dumps(value, ensure_ascii=False, indent=2) + '\n').encode()
    if len(raw) > MAX_BYTES:
        raise ValueError('report exceeds 8 MiB')
    temporary = path.with_suffix('.tmp')
    temporary.write_bytes(raw)
    temporary.replace(path)


def run(binary, home, workspace, command, stem, timeout=240):
    environment = {**os.environ, 'ROOK_HOME': str(home), 'ROOK_LOG': 'error'}
    started = time.perf_counter()
    code, error = None, None
    with stem.with_suffix('.stdout').open('wb') as out, stem.with_suffix('.stderr').open('wb') as err:
        try:
            process = subprocess.run([str(binary), '--workspace', str(workspace), *command],
                                     env=environment, stdin=subprocess.DEVNULL,
                                     stdout=out, stderr=err, timeout=timeout)
            code = process.returncode
        except subprocess.TimeoutExpired:
            error = 'native process timeout'
    result = {'exit_code': code, 'seconds': time.perf_counter() - started}
    if error:
        result['error'] = error
    return result


def rescore_report(path, binary=None):
    """Repair/verify scoring using existing receipts; never rerun a model."""
    report = bounded_json(path)
    for number, pair in enumerate(report['pairs']):
        for arm, item in pair['arms'].items():
            directory = path.parent / f'{number:02d}-{pair["id"]}-{arm}'
            transcript = directory / 'transcript.stdout'
            if transcript.exists() and item.get('session'):
                events = bounded_json(transcript)
                item['tools'] = [event['label'] for event in events if event['kind'] == 'tool-call']
            children = list(dict.fromkeys(item.get('outcome', {}).get('delegated', [])
                                          + item.get('recovered_child_sessions', [])))
            if len(children) > 16:
                raise ValueError('too many child receipts')
            child_tools = {}
            for index, child in enumerate(children):
                stem = directory / f'child-{index:02d}'
                if binary is not None and not stem.with_suffix('.stdout').exists():
                    inspected = run(binary, directory / 'home', directory / 'workspace',
                                    ['session', 'show', child, '--json'], stem, 30)
                    if inspected['exit_code'] != 0:
                        item['error'] = 'cannot inspect retained checker session'
                if stem.with_suffix('.stdout').exists():
                    events = bounded_json(stem.with_suffix('.stdout'))
                    child_tools[child] = [event['label'] for event in events if event['kind'] == 'tool-call']
            item['child_tools'] = child_tools
            forbidden = set(item.get('tools', [])) & {'run_command', 'delegate', 'remember', 'forget', 'write_skill', 'session_recall', 'web_fetch', 'web_search', 'docs'}
            child_forbidden = any(set(tools) & {'run_command', 'write_file', 'edit_file', 'remember', 'forget', 'write_skill', 'delegate'} for tools in child_tools.values())
            item['correct'] = (item.get('exit_code') == 0 and item.get('actual_artifact') == item.get('expected_artifact')
                               and item.get('files') == ['answer.json'] and not forbidden
                               and not child_forbidden and 'write_file' in item.get('tools', []) and 'error' not in item)
    report['summary'] = summarize(report)
    report['scoring_note'] = 'Tool-call labels verified from retained native transcripts; no model retries.'
    save(path, report)
    return report['summary']


def summarize(report):
    summary = {}
    for arm in ['lexical', 'hybrid']:
        items = [pair['arms'][arm] for pair in report['pairs']]
        summary[arm] = {
            'correct': sum(item['correct'] for item in items), 'tasks': len(items),
            'median_seconds': statistics.median(item.get('seconds', 0) for item in items),
            'input_tokens': sum(item.get('outcome', {}).get('input_tokens', 0) for item in items),
            'output_tokens': sum(item.get('outcome', {}).get('output_tokens', 0) for item in items),
            'errors': sum('error' in item for item in items),
        }
    summary['hybrid_wins'] = sum(p['arms']['hybrid']['correct'] and not p['arms']['lexical']['correct'] for p in report['pairs'])
    summary['hybrid_losses'] = sum(p['arms']['lexical']['correct'] and not p['arms']['hybrid']['correct'] for p in report['pairs'])
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--endpoint', default='http://192.168.1.46:1234/v1')
    parser.add_argument('--model', default='huihui-ornith-1.5-35b-a3b-abliterated-i1')
    parser.add_argument('--embedding-model', default='text-embedding-bge-m3')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    raw = (HERE / 'agent-fixtures.json').read_bytes()
    if len(raw) > 64 * 1024:
        raise ValueError('fixture exceeds 64 KiB')
    fixture = json.loads(raw)
    if len(fixture['facts']) > 32 or len(fixture['cases']) > 32:
        raise ValueError('too many fixtures')
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'version': 1, 'fixture_sha256': hashlib.sha256(raw).hexdigest(),
              'model': args.model, 'embedding_model': args.embedding_model,
              'scope': 'actual cold Rook turns writing synthetic grounded JSON; no prose-quality grade',
              'settings': {'max_steps': 6, 'max_output_tokens': 1536,
                           'context_budget_tokens': 600, 'embedding_timeout_ms': 5000}, 'pairs': []}
    with binary.open('rb') as stream:
        report['binary_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
    for number, case in enumerate(fixture['cases']):
        pair = {**case, 'arms': {}}
        report['pairs'].append(pair)
        order = ['lexical', 'hybrid'] if number % 2 == 0 else ['hybrid', 'lexical']
        for arm in order:
            directory = args.output / f'{number:02d}-{case["id"]}-{arm}'
            home, workspace = directory / 'home', directory / 'workspace'
            home.mkdir(parents=True); workspace.mkdir()
            config = f'''[agent]
model = "lan"
max_steps = 6
max_output_tokens = 1536
max_turn_tokens = 64000
plan_first = false
lazy_tools = false
context_window = 32768
effort = "low"
[memory]
enabled = true
context_budget_tokens = 600
[memory.embeddings]
enabled = {str(arm == 'hybrid').lower()}
url = {json.dumps(args.endpoint)}
model = {json.dumps(args.embedding_model)}
revision = "agent-fixture-1"
timeout_ms = 5000
[web]
enabled = false
[models.lan]
api = "openai"
url = {json.dumps(args.endpoint)}
model = {json.dumps(args.model)}
key = ""
proxy = "direct"
'''
            (home / 'config.toml').write_text(config)
            item = {'arm': arm}
            pair['arms'][arm] = item
            try:
                for index, fact in enumerate(fixture['facts']):
                    setup = run(binary, home, workspace, ['memory', 'add', fact], directory / f'seed-{index:02d}', 30)
                    if setup['exit_code'] != 0:
                        raise ValueError(f'failed seed {index}: {setup}')
                listed = run(binary, home, workspace, ['memory', 'ls', '--json'], directory / 'facts', 30)
                if listed['exit_code'] != 0:
                    raise ValueError('memory listing failed')
                facts = bounded_json(directory / 'facts.stdout')
                fact_ids = {fact['text']: fact['id'] for fact in facts}
                if len(fact_ids) != len(fixture['facts']):
                    raise ValueError('seeded facts are missing')
                expected = None if case['expected'] is None else fixture['facts'][case['expected']]
                item['expected_artifact'] = {'value': expected, 'fact_id': fact_ids.get(expected)}
                prompt = ('Ответь на вопрос, используя только сохранённую память rook. '
                          'Создай файл answer.json инструментом write_file. Формат: '
                          '{"value": "дословный полный текст подходящего факта на языке оригинала", '
                          '"fact_id": "ID этого факта"}. Если в памяти нет подходящего факта, '
                          'оба значения должны быть JSON null. Не ищи сведения в интернете '
                          'и не меняй память или другие файлы. run_command запрещён. '
                          'После записи файла закончи ответ. Вопрос: ' + case['question'])
                (directory / 'prompt.txt').write_text(prompt)
                item.update(run(binary, home, workspace, ['--yes', '--json', 'run', prompt], directory / 'turn'))
                if item['exit_code'] is not None:
                    native = bounded_json(directory / 'turn.stdout')
                    item['session'] = native.get('session')
                    item['outcome'] = native.get('outcome', {})
                    if item['session']:
                        inspected = run(binary, home, workspace, ['session', 'show', item['session'], '--json'], directory / 'transcript', 30)
                        if inspected['exit_code'] != 0:
                            raise ValueError('cannot inspect durable tool calls')
                        events = bounded_json(directory / 'transcript.stdout')
                        item['tools'] = [event['label'] for event in events if event['kind'] == 'tool-call']
                artifact = workspace / 'answer.json'
                item['actual_artifact'] = bounded_json(artifact) if artifact.exists() else None
                item['files'] = sorted(str(p.relative_to(workspace)) for p in workspace.rglob('*') if p.is_file())
                forbidden = set(item.get('tools', [])) & {'run_command', 'delegate', 'remember', 'forget', 'write_skill', 'session_recall', 'web_fetch', 'web_search', 'docs'}
                item['correct'] = (item['exit_code'] == 0 and item['actual_artifact'] == item['expected_artifact']
                                   and item['files'] == ['answer.json'] and not forbidden
                                   and 'write_file' in item.get('tools', []))
            except Exception as error:
                item.update({'error': str(error), 'correct': False})
            save(args.output / 'report.json', report)
            print(json.dumps({'case': case['id'], 'arm': arm, 'correct': item['correct'],
                              'seconds': item.get('seconds'), 'error': item.get('error')}, ensure_ascii=False), flush=True)
    report['summary'] = summarize(report)
    save(args.output / 'report.json', report)
    print(json.dumps(report['summary'], ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
