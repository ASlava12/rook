"""Validate Ollama preparation responses; HTTP 200 alone is not pull success."""
import argparse
import json
import sys
import time

MAX_LINE = 64 * 1024
MAX_BYTES = 16 * 1024 * 1024
MAX_RECORDS = 50000
MAX_INVENTORY = 1024 * 1024


def pull(stream, report=lambda text: print(text, flush=True)):
    used = records = 0
    status = None
    previous = None
    announced = time.monotonic()
    while True:
        line = stream.readline(MAX_LINE + 1)
        if not line:
            break
        used += len(line)
        if len(line) > MAX_LINE or used > MAX_BYTES:
            raise ValueError('pull progress exceeds its byte limit')
        if not line.strip():
            continue
        records += 1
        if records > MAX_RECORDS:
            raise ValueError('pull progress exceeds its record limit')
        event = json.loads(line)
        if not isinstance(event, dict):
            raise ValueError('pull progress must be an object')
        if 'error' in event:
            raise ValueError('pull returned an error: ' + json.dumps(event['error'])[:512])
        status = event.get('status')
        if not isinstance(status, str) or not status or len(status) > 512:
            raise ValueError('pull progress has no bounded status')
        now = time.monotonic()
        if status != 'success' and (status != previous or now - announced >= 30):
            report('model pull: ' + json.dumps(status))
            announced = now
        previous = status
    if status != 'success':
        raise ValueError('pull ended without a final success status')
    report('model pull completed')


def inventory(stream, model):
    body = stream.read(MAX_INVENTORY + 1)
    if len(body) > MAX_INVENTORY:
        raise ValueError('model inventory exceeds its byte limit')
    response = json.loads(body)
    if not isinstance(response, dict) or 'error' in response:
        raise ValueError('model inventory is an error or is not an object')
    models = response.get('models')
    if not isinstance(models, list):
        raise ValueError('model inventory has no model list')
    if not any(isinstance(item, dict) and model in (item.get('name'), item.get('model')) for item in models):
        raise ValueError('the requested model is absent after the pull')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('response', choices=['pull', 'tags'])
    parser.add_argument('--model')
    args = parser.parse_args()
    if args.response == 'tags' and not args.model:
        parser.error('tags requires --model')
    try:
        if args.response == 'pull':
            pull(sys.stdin.buffer)
        else:
            inventory(sys.stdin.buffer, args.model)
            print('requested model is available', flush=True)
    except (ValueError, OSError) as error:
        print('model preparation failed: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
