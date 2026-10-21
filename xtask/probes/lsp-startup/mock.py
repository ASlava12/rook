import json
import os
from pathlib import Path
import sys
import time

language, log_name, gate_name = sys.argv[1:]
log = Path(log_name)
def record(value):
    encoded = json.dumps(dict(language=language, pid=os.getpid(), **value)) + '\n'
    with log.open('a', encoding='utf-8') as sink:
        assert sink.tell() + len(encoded.encode()) <= 64 * 1024
        sink.write(encoded)
def send(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
    sys.stdout.buffer.flush()
while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline(4097)
        if not line:
            raise SystemExit(0)
        assert len(line) <= 4096
        if line in (b'\r\n', b'\n'):
            break
        name, value = line.decode().split(':', 1)
        headers[name.lower()] = value.strip()
    size = int(headers['content-length'])
    assert 0 < size <= 1024 * 1024
    message = json.loads(sys.stdin.buffer.read(size))
    method = message.get('method', '')
    if method == 'initialize':
        record({'method': method, 'root': message['params'].get('rootUri')})
        if gate_name != '-':
            deadline = time.monotonic() + 180
            while not Path(gate_name).exists():
                assert time.monotonic() < deadline
                time.sleep(.01)
        result = {'capabilities': {'textDocumentSync': 1, 'workspaceSymbolProvider': True}}
    elif method == 'exit':
        break
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        uri = message['params']['textDocument']['uri']
        record({'method': method, 'uri': uri})
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics',
              'params': {'uri': uri, 'diagnostics': []}})
        continue
    else:
        result = [] if method == 'workspace/symbol' else None
        if method == 'workspace/symbol':
            record({'method': method, 'query': message['params']['query']})
    if 'id' in message:
        send({'jsonrpc': '2.0', 'id': message['id'], 'result': result})
