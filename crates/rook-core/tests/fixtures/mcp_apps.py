#!/usr/bin/env python3
"""An owned app server: the call counter proves no consent/retry leaks."""
import json,sys,time
log=sys.argv[1]
html='''<!doctype html><html><body><h1>Rook app fixture</h1><button id="refresh">Refresh count</button><p id="count">0</p><script>
let seq=1;const wait=new Map();function rpc(method,params){const id=seq++;return new Promise((ok,no)=>{wait.set(id,{ok,no});parent.postMessage({jsonrpc:'2.0',id,method,params},'*');});}
addEventListener('message',e=>{if(e.source!==parent)return;const p=wait.get(e.data.id);if(p){wait.delete(e.data.id);e.data.error?p.no(e.data.error):p.ok(e.data.result);}});
rpc('ui/initialize',{protocolVersion:'2026-01-26',appInfo:{name:'fixture',version:'1'},appCapabilities:{}}).then(()=>{document.body.dataset.ready='yes';parent.postMessage({jsonrpc:'2.0',method:'ui/notifications/initialized'},'*');});
document.querySelector('#refresh').onclick=()=>rpc('tools/call',{name:'count',arguments:{label:'fixture'}}).then(r=>document.querySelector('#count').textContent=r.content[0].text);
</script></body></html>'''
apps=False
for line in sys.stdin:
 request=json.loads(line)
 if 'id' not in request:continue
 method=request['method'];params=request.get('params',{})
 if method=='initialize':
  apps='io.modelcontextprotocol/ui' in params.get('capabilities',{}).get('extensions',{})
  result={'protocolVersion':'2025-03-26','capabilities':{'tools':{},'resources':{}},'serverInfo':{'name':'apps-fixture','version':'1'}}
 elif method=='tools/list':
  tool={'name':'count','description':'Fixture counter; text fallback supported.','inputSchema':{'type':'object','properties':{'label':{'type':'string'}},'required':['label'],'additionalProperties':False}}
  if apps:tool['_meta']={'ui':{'resourceUri':'ui://fixture/view','visibility':['model','app']}}
  result={'tools':[tool]}
 elif method=='resources/read':
  result={'contents':[{'uri':params['uri'],'mimeType':'text/html;profile=mcp-app','text':html}]}
 elif method=='tools/call':
  with open(log,'a') as output:output.write(json.dumps(params)+'\n')
  count=len(open(log).readlines())
  result={'content':[{'type':'text','text':str(count)}]}
 else:result={}
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
