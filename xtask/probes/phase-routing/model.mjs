import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

const root = path.resolve(process.argv[2]);
const steps = new Map();
let requests = 0;
const server = http.createServer(async (req, res) => {
  if (req.method === 'GET') {
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({data:[]})); return;
  }
  const chunks = []; let bytes = 0;
  for await (const chunk of req) {
    bytes += chunk.length;
    if (bytes > 4 * 1024 * 1024) { res.writeHead(413); res.end(); return; }
    chunks.push(chunk);
  }
  assert(++requests <= 48, 'fixture request budget exhausted');
  const request = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  fs.writeFileSync(path.join(root, `request-${String(requests).padStart(3, '0')}.json`), JSON.stringify(request));
  const user = request.messages.filter(m => m.role === 'user').map(m => m.content).join('\n');
  const tag = [...user.matchAll(/(?:LOCAL|SHARED|BROWSER)_PHASE/g)].at(-1)?.[0] ?? 'SEED';
  const step = request.stream ? (steps.get(tag) ?? 0) + 1 : 0;
  if (request.stream) steps.set(tag, step);
  const write = request.stream && tag !== 'SEED' && step === 1 && request.model === 'analysis-model';
  const message = !request.stream ? {role:'assistant',content:'{"action":"finish"}'} : write ?
    {role:'assistant',content:'',tool_calls:[{index:0,id:`write-${tag}`,type:'function',function:{name:'write_file',arguments:JSON.stringify({path:'routing.txt',content:`${tag}\n`})}}]} :
    {role:'assistant',content:`${request.model === 'analysis-model' ? 'ANALYSIS' : 'IMPLEMENTATION'}_${tag}`};
  const answer = {model:`server-${request.model}`,choices:[{index:0,[request.stream?'delta':'message']:message,finish_reason:write?'tool_calls':'stop'}],usage:{prompt_tokens:10,completion_tokens:3}};
  res.setHeader('Content-Type', request.stream ? 'text/event-stream' : 'application/json');
  res.setHeader('Connection', 'close');
  if (!request.stream) { res.end(JSON.stringify(answer)); return; }
  fs.writeFileSync(path.join(root, `started-${tag}-${step}`), request.model);
  if (!write && tag !== 'SEED' && step === 2) {
    // Deliver visible output before terminal usage, so the inspector must keep
    // the preceding historical receipt separate from this current attempt.
    res.write(`data: ${JSON.stringify({model:answer.model,choices:[{index:0,delta:message,finish_reason:null}]})}\n\n`);
    const deadline = Date.now() + 120000;
    while (!fs.existsSync(path.join(root, `release-${tag}`)) && !res.destroyed && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    if (res.destroyed) return;
    assert(Date.now() < deadline, 'fixture release deadline exceeded');
    answer.choices[0].delta = {};
  }
  res.end(`data: ${JSON.stringify(answer)}\n\ndata: [DONE]\n\n`);
});
server.listen(0, '127.0.0.1', () => fs.writeFileSync(path.join(root, 'model-address'), `http://127.0.0.1:${server.address().port}`));
