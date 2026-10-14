#!/usr/bin/env python3
"""Protocol fixture for cancellation/isolation tests; real Chromium is tested separately."""
import json
import os
import subprocess
import sys
import time

log = sys.argv[1]
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(3600)'],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
with open(log, 'a') as output:
    output.write(json.dumps({'child': child.pid}) + '\n')
url = ''
names = ['browser_navigate', 'browser_snapshot', 'browser_click', 'browser_type',
         'browser_wait_for', 'browser_take_screenshot', 'browser_close']
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion': '2025-03-26', 'capabilities': {'tools': {}},
                  'serverInfo': {'name': 'browser-fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': name, 'inputSchema': {'type': 'object'}} for name in names]}
    elif method == 'tools/call':
        parameters = request['params']
        name, args = parameters['name'], parameters['arguments']
        with open(log, 'a') as output:
            output.write(json.dumps(parameters) + '\n')
        if name == 'browser_navigate':
            url = args['url']
            if url.endswith('/crash'):
                os._exit(0)
        if name == 'browser_wait_for':
            time.sleep(60)
        text = url + '\n- textbox "Message" [ref=e1]\n- button "Save" [ref=e2]'
        result = {'content': [{'type': 'text', 'text': text}]}
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
