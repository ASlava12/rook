#!/usr/bin/env python3
"""Read retained native receipts; never rerun the model or overwrite the run."""
import argparse
import hashlib
import json
from pathlib import Path


def load(path):
    if path.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('receipt exceeds the inspection ceiling')
    return json.loads(path.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run', type=Path)
    args = parser.parse_args()
    report = load(args.run / 'report.json')
    if 'completed_wins' not in report or 'completed_losses' not in report:
        raise ValueError('native comparison is not terminal; inspect the live handle before analysing')
    derived = {'report_sha256': hashlib.sha256((args.run / 'report.json').read_bytes()).hexdigest(),
               'pairs': [], 'note': 'Program calls counted from actual child tool-call events. Partial transcript bodies are not parsed as complete results.'}
    for number, pair in enumerate(report['pairs']):
        measured = {'id': pair['id'], 'arms': {}}
        derived['pairs'].append(measured)
        for arm, item in pair['arms'].items():
            root = args.run / f'{number:02d}-{pair["id"]}-{arm}'
            frames = []
            for transcript in sorted(root.glob('transcript-*.stdout')):
                events = load(transcript)
                if not any(event['label'] == 'rook:code-mode-source' for event in events):
                    continue
                calls = [event['label'] for event in events if event['kind'] == 'tool-call']
                frames.append({'transcript': transcript.name, 'nested_calls': len(calls), 'tools': calls,
                               'inspection_may_be_paged': len(events) >= 200})
            measured['arms'][arm] = {'programs': frames, 'nested_calls': sum(frame['nested_calls'] for frame in frames),
                                     'value_correct': item.get('value_correct', False),
                                     'completed_correct': item.get('completed_correct', False)}
    output = args.run / 'program-analysis.json'
    if output.exists():
        raise ValueError('derived analysis already exists; retain it and use a new destination')
    output.write_text(json.dumps(derived, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(derived, ensure_ascii=False))


if __name__ == '__main__':
    main()
