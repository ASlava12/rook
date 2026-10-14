#!/usr/bin/env python3
"""Compare actual Rook lexical retrieval with local embeddings on fixed fixtures.

No user memory is read. Results are measurements of retrieval, not LLM answer
quality; they cannot by themselves justify changing default agent behavior.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import statistics
import subprocess
import time
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
LIMIT = 8 * 1024 * 1024


def normalized(vector):
    if not vector or len(vector) > 4096 or not all(math.isfinite(x) for x in vector):
        raise ValueError('invalid embedding dimension or nonfinite values')
    norm = math.sqrt(sum(x * x for x in vector))
    if not math.isfinite(norm) or norm <= 0:
        raise ValueError('zero or nonfinite embedding norm')
    return [x / norm for x in vector]


def embeddings(client, endpoint, model, texts):
    body = json.dumps({'model': model, 'input': texts}).encode()
    started = time.perf_counter()
    request = urllib.request.Request(endpoint.rstrip('/') + '/embeddings', data=body,
                                     headers={'Content-Type': 'application/json'})
    with client.open(request, timeout=60) as response:
        raw = response.read(LIMIT + 1)
    if len(raw) > LIMIT:
        raise ValueError('embedding response exceeds 8 MiB')
    data = json.loads(raw)
    rows = sorted(data['data'], key=lambda row: row['index'])
    if [row['index'] for row in rows] != list(range(len(texts))):
        raise ValueError('missing or duplicate embedding rows')
    vectors = [normalized(row['embedding']) for row in rows]
    if len({len(v) for v in vectors}) != 1:
        raise ValueError('inconsistent embedding dimensions')
    return vectors, time.perf_counter() - started


def metrics(cases):
    def summarize(items):
        relevant = [c for c in items if c['expected'] is not None]
        irrelevant = [c for c in items if c['expected'] is None]
        return {'count': len(items),
                'top1': sum(c['ranking'][:1] == [c['expected']] for c in relevant),
                'top3': sum(c['expected'] in c['ranking'][:3] for c in relevant),
                'relevant': len(relevant),
                'mrr': sum(1 / (c['ranking'].index(c['expected']) + 1)
                           if c['expected'] in c['ranking'] else 0 for c in relevant) / max(1, len(relevant)),
                'irrelevant_false_hits': sum(bool(c['ranking']) for c in irrelevant)}
    result = summarize(cases)
    result['groups'] = {group: summarize([c for c in cases if c['group'] == group])
                        for group in sorted({c['group'] for c in cases})}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--endpoint', default='http://192.168.1.46:1234/v1')
    parser.add_argument('--model', action='append', default=[])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = (HERE / 'fixtures.json').read_bytes()
    corpus = json.loads(raw)
    run = subprocess.run(['cargo', 'run', '--quiet', '-p', 'rook-core', '--example', 'memory-retrieval'],
                         cwd=ROOT, input=raw, stdout=subprocess.PIPE, check=True)
    lexical = json.loads(run.stdout)
    queries, facts = corpus['queries'], corpus['facts']
    if len(lexical) != len(queries):
        raise ValueError('baseline omitted queries')
    baseline = [{**q, 'ranking': [h['id'] for h in result['hits']], 'micros': result['micros']}
                for q, result in zip(queries, lexical)]
    report = {'version': 1, 'fixture_sha256': hashlib.sha256(raw).hexdigest(),
              'facts': len(facts), 'queries': len(queries),
              'fusion': '0.8*cosine + 0.2*normalized lexical overlap; literal-query boost; cosine >= 0.5',
              'scope': 'synthetic retrieval only; no LLM answer-quality evidence',
              'lexical': {'metrics': metrics(baseline), 'cases': baseline,
                          'median_micros': statistics.median(c['micros'] for c in baseline)},
              'models': []}
    args.output.mkdir(parents=True, exist_ok=True)
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for model in args.model or ['text-embedding-bge-m3', 'text-embedding-nomic-embed-text-v1.5']:
        item = {'model': model}
        try:
            prefixed = 'nomic' in model
            document_texts = [('search_document: ' if prefixed else '') + f['text'] for f in facts]
            docs, indexing = embeddings(client, args.endpoint, model, document_texts)
            cases, vector_cases, latency = [], [], []
            for query, original in zip(queries, lexical):
                qtext = ('search_query: ' if prefixed else '') + query['text']
                vectors, duration = embeddings(client, args.endpoint, model, [qtext])
                latency.append(duration)
                scores = {h['id']: h['score'] for h in original['hits']}
                max_lex = max(scores.values(), default=1)
                ranked = []
                for fact, vector in zip(facts, docs):
                    if len(vector) != len(vectors[0]):
                        raise ValueError('query/document embedding dimension mismatch')
                    cosine = sum(a * b for a, b in zip(vector, vectors[0]))
                    literal = query['text'].casefold() in fact['text'].casefold()
                    lex = scores.get(fact['id'], 0) / max_lex
                    if cosine >= 0.5 or lex > 0 or literal:
                        ranked.append({'id': fact['id'], 'cosine': cosine,
                                       'score': 0.8 * cosine + 0.2 * lex + (2 if literal else 0)})
                ranked.sort(key=lambda r: -r['score'])
                cases.append({**query, 'ranking': [r['id'] for r in ranked], 'seconds': duration,
                              'top_scores': ranked[:3]})
                vector_cases.append({**query, 'ranking': [r['id'] for r in sorted(ranked, key=lambda r: -r['cosine']) if r['cosine'] >= 0.5]})
            item.update({'dimension': len(docs[0]), 'index_seconds': indexing,
                         'median_query_seconds': statistics.median(latency), 'max_query_seconds': max(latency),
                         'cache_bytes_f32': len(docs) * len(docs[0]) * 4,
                         'hybrid_metrics': metrics(cases), 'dense_metrics': metrics(vector_cases), 'cases': cases})
            print(json.dumps({k: v for k, v in item.items() if k != 'cases'}, ensure_ascii=False), flush=True)
        except Exception as error:
            item['error'] = str(error)
            print(json.dumps(item), flush=True)
        report['models'].append(item)
        (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print('lexical:', json.dumps(report['lexical']['metrics']), flush=True)
    print('report:', args.output / 'report.json', flush=True)


if __name__ == '__main__':
    main()
