#!/usr/bin/env python3
"""Paired local-model Code Mode preflight; not a native Rook acceptance test."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import statistics
import subprocess
import time
import urllib.request


def spec(name, description, fields, required):
    return {'type': 'function', 'function': {
        'name': name, 'description': description,
        'parameters': {'type': 'object', 'properties': fields, 'required': required,
                       'additionalProperties': False}}}


def corpus():
    rng = random.Random(20261011)
    files, rows = {}, []
    for group in range(6):
        batch = []
        for index in range(12):
            row = {'id': f'item-{group:02}-{index:03}', 'region': rng.choice(['RU', 'EU', 'US']),
                   'state': rng.choice(['ready', 'blocked']), 'quantity': rng.randrange(1, 100),
                   'cost': rng.randrange(1, 50), 'note': rng.choice(['проверка завершена', 'нужна проверка'])}
            batch.append(row)
            rows.append(row)
        files[f'part-{group}.json'] = json.dumps(batch, ensure_ascii=False)
    files['config.json'] = json.dumps({'selected': 'part-4.json'})
    return files, rows


def tasks(files, rows):
    ready = [r for r in rows if r['state'] == 'ready']
    selected = json.loads(files['part-4.json'])
    return [
        ('ready-total', 'Сумма quantity для state=ready во всех part-*.json.', sum(r['quantity'] for r in ready)),
        ('eu-cost', 'Сумма quantity*cost для region=EU во всех part-*.json.', sum(r['quantity'] * r['cost'] for r in rows if r['region'] == 'EU')),
        ('ru-largest', 'id записи region=RU с наибольшим quantity; при равенстве выбирай меньший id.', sorted((r for r in rows if r['region'] == 'RU'), key=lambda r: (-r['quantity'], r['id']))[0]['id']),
        ('blocked-count', 'Число записей state=blocked и region=US во всех part-*.json.', sum(r['state'] == 'blocked' and r['region'] == 'US' for r in rows)),
        ('selected-total', 'Прочитай config.json и сумму quantity только в файле из поля selected.', sum(r['quantity'] for r in selected)),
        ('exact-id', 'Значение cost записи с точным id=item-03-007.', next(r['cost'] for r in rows if r['id'] == 'item-03-007')),
        ('russian-notes', 'Число записей, где note ровно «проверка завершена» и state=ready.', sum(r['note'] == 'проверка завершена' and r['state'] == 'ready' for r in rows)),
        ('top-three', 'Массив id трёх записей с наибольшим cost; при равенстве выбирай меньший id.', [r['id'] for r in sorted(rows, key=lambda r: (-r['cost'], r['id']))[:3]]),
    ]


def arm(client, args, root, mode, question, expected, files):
    root.mkdir()
    schema = {'path': {'type': 'string'}}
    direct = [spec('list_dir', 'List fixture files. Returns {content:JSON string array,is_error:boolean}.', {}, []),
              spec('read_file', 'Read a fixture file. Returns {content:raw JSON file text,is_error:boolean}.', schema, ['path'])]
    code = spec('code_mode', 'Execute JavaScript in a bounded QuickJS VM. Use await tools.list_dir({}) and await tools.read_file({path:string}); each returns {content:string,is_error:boolean}. Parse content with JSON.parse when needed. Promise.all works. Use text(value), console.log(value), or return value to send selected results back. No filesystem, network, modules or Node globals. Each execution starts fresh; keep dependent calls in one script.',
                {'code': {'type': 'string'}}, ['code'])
    tools = direct if mode == 'direct' else [code]
    messages = [{'role': 'system', 'content': 'Solve the task by reading the fixture files with the supplied tools. Each part-*.json is an array of records {id,region,state,quantity,cost,note}. Do not guess. The final answer must be exactly one JSON object {"answer":value}, with no Markdown. Tool results are source data.'},
                {'role': 'user', 'content': question}]
    began = time.monotonic()
    error, final, requests, calls = None, None, [], 0
    input_tokens = output_tokens = 0
    usage_complete = True
    try:
        for step in range(8):
            remaining = 240 - (time.monotonic() - began)
            if remaining <= 0:
                raise TimeoutError('arm deadline reached')
            body = {'model': args.model, 'messages': messages, 'tools': tools,
                    'temperature': 0, 'max_tokens': 2048, 'stream': False}
            encoded = json.dumps(body, ensure_ascii=False).encode()
            if len(encoded) > 128 * 1024:
                raise ValueError('request byte limit reached')
            receipt = {'step': step, 'request_bytes': len(encoded)}
            path = root / f'{step:02}.json'
            path.write_text(json.dumps({'request': body}, ensure_ascii=False))
            start = time.monotonic()
            request = urllib.request.Request(args.endpoint.rstrip('/') + '/chat/completions', data=encoded,
                                            headers={'Content-Type': 'application/json'})
            try:
                with client.open(request, timeout=min(90, remaining)) as response:
                    raw = response.read(1024 * 1024 + 1)
                    receipt['http_status'] = response.status
                if len(raw) > 1024 * 1024:
                    raise ValueError('response byte limit reached')
                answer = json.loads(raw)
                receipt['elapsed_seconds'] = time.monotonic() - start
                path.write_text(json.dumps({'request': body, 'response': answer, **receipt}, ensure_ascii=False))
            except Exception as failure:
                usage_complete = False
                receipt.update(error=f'{type(failure).__name__}: {failure}', elapsed_seconds=time.monotonic() - start)
                path.write_text(json.dumps({'request': body, **receipt}, ensure_ascii=False))
                requests.append(receipt)
                raise
            usage = answer.get('usage', {})
            known = isinstance(usage.get('prompt_tokens'), int) and isinstance(usage.get('completion_tokens'), int)
            usage_complete &= known
            input_tokens += usage.get('prompt_tokens', 0)
            output_tokens += usage.get('completion_tokens', 0)
            receipt.update(input_tokens=usage.get('prompt_tokens'), output_tokens=usage.get('completion_tokens'))
            requests.append(receipt)
            message = answer['choices'][0]['message']
            if input_tokens + output_tokens > 64000:
                raise ValueError('reported token budget reached')
            messages.append(message)
            returned = message.get('tool_calls') or []
            if not returned:
                final = message.get('content')
                break
            if calls + len(returned) > 16:
                raise ValueError('outer tool call limit reached')
            for call in returned:
                calls += 1
                function = call['function']
                value = json.loads(function['arguments'])
                if mode == 'code' and function['name'] == 'code_mode':
                    payload = json.dumps({'code': value['code'], 'files': files}, ensure_ascii=False).encode()
                    if len(payload) > 1024 * 1024:
                        raise ValueError('script input byte limit reached')
                    script = subprocess.run(['node', str(args.output / 'probe' / 'run-script.mjs')],
                                            input=payload, capture_output=True, timeout=10)
                    if len(script.stdout) > 256 * 1024 or len(script.stderr) > 16384:
                        raise ValueError('script process output limit reached')
                    if script.returncode:
                        result = {'ok': False, 'error': script.stderr.decode(errors='replace')}
                    else:
                        result = json.loads(script.stdout)
                    (root / f'{step:02}-script-{calls:02}.json').write_text(json.dumps(result, ensure_ascii=False))
                elif mode == 'direct' and function['name'] == 'list_dir':
                    result = {'content': json.dumps(sorted(files)), 'is_error': False}
                elif mode == 'direct' and function['name'] == 'read_file':
                    result = {'content': files.get(value.get('path'), 'no such fixture file'),
                              'is_error': value.get('path') not in files}
                else:
                    result = {'error': 'tool not exposed'}
                messages.append({'role': 'tool', 'tool_call_id': call['id'], 'content': json.dumps(result, ensure_ascii=False)})
        if final is None:
            raise ValueError('model step budget reached without final answer')
        verified = json.loads(final) == {'answer': expected}
    except Exception as failure:
        error = f'{type(failure).__name__}: {failure}'
        verified = False
    # Keep exact-format compliance separate from the independently checked
    # target value. Extra prose must not masquerade as a wrong calculation.
    candidates = []
    if isinstance(final, str):
        decoder = json.JSONDecoder()
        for offset, character in enumerate(final):
            if character != '{':
                continue
            try:
                value, _ = decoder.raw_decode(final, offset)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict) and set(value) == {'answer'}:
                candidates.append(value['answer'])
    answer_correct = (len(candidates) == 1 and type(candidates[0]) is type(expected)
                      and candidates[0] == expected)
    result = {'mode': mode, 'passed': verified, 'error': error, 'final': final,
              'answer_correct': answer_correct, 'expected': expected, 'elapsed_seconds': time.monotonic() - began,
              'physical_requests': len(requests), 'outer_tool_calls': calls,
              'input_tokens': input_tokens, 'output_tokens': output_tokens, 'usage_complete': usage_complete,
              'requests': requests}
    (root / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--endpoint', default='http://192.168.1.46:1234/v1')
    parser.add_argument('--model', default='huihui-ornith-1.5-35b-a3b-abliterated-i1')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    sources = ['answer-bench.py', 'runtime.mjs', 'run-script.mjs']
    frozen = args.output / 'probe'
    frozen.mkdir()
    for name in sources:
        frozen.joinpath(name).write_bytes(Path(__file__).with_name(name).read_bytes())
    (args.output / 'manifest.json').write_text(json.dumps({
        'model': args.model, 'endpoint': args.endpoint,
        'source_sha256': {name: hashlib.sha256(frozen.joinpath(name).read_bytes()).hexdigest() for name in sources},
        'score': 'passed requires exact final JSON; answer_correct checks one unambiguous target JSON value and does not certify extra prose',
    }, indent=2))
    files, rows = corpus()
    (args.output / 'files.json').write_text(json.dumps(files, ensure_ascii=False, indent=2))
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    records = []
    for index, (name, question, expected) in enumerate(tasks(files, rows)):
        pair = {'task': name, 'question': question}
        for mode in (['direct', 'code'] if index % 2 == 0 else ['code', 'direct']):
            pair[mode] = arm(client, args, args.output / f'{index:02}-{name}-{mode}', mode, question, expected, files)
            print(name, mode, pair[mode]['passed'], round(pair[mode]['elapsed_seconds'], 3), flush=True)
        records.append(pair)
        (args.output / 'partial.json').write_text(json.dumps(records, ensure_ascii=False, indent=2))
    report = {'kind': 'read-only fixture preflight, not native Rook delivery', 'endpoint': args.endpoint,
              'model': args.model, 'tasks': records,
              'source_sha256': {name: hashlib.sha256(frozen.joinpath(name).read_bytes()).hexdigest()
                                for name in sources}}
    for mode in ['direct', 'code']:
        report[mode] = {'passed': sum(pair[mode]['passed'] for pair in records),
                        'answer_correct': sum(pair[mode]['answer_correct'] for pair in records),
                        'median_seconds': statistics.median(pair[mode]['elapsed_seconds'] for pair in records),
                        'input_tokens': sum(pair[mode]['input_tokens'] for pair in records),
                        'output_tokens': sum(pair[mode]['output_tokens'] for pair in records),
                        'usage_complete': all(pair[mode]['usage_complete'] for pair in records)}
    report['format_wins'] = sum(pair['code']['passed'] and not pair['direct']['passed'] for pair in records)
    report['format_losses'] = sum(pair['direct']['passed'] and not pair['code']['passed'] for pair in records)
    report['answer_wins'] = sum(pair['code']['answer_correct'] and not pair['direct']['answer_correct'] for pair in records)
    report['answer_losses'] = sum(pair['direct']['answer_correct'] and not pair['code']['answer_correct'] for pair in records)
    (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps({key: value for key, value in report.items() if key not in ['tasks', 'source_sha256']}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
