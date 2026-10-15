import sys,json,time,urllib.request,urllib.error,secrets,statistics
key=json.loads(sys.stdin.read(16384))['key']
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'}
base='http://127.0.0.1:8080/v1/chat/completions'
run=secrets.token_hex(6)
def measure(prefix,arm,round_no,effort='none'):
 body={'model':'qwen3.8-flash-next','messages':[{'role':'system','content':prefix},{'role':'user','content':'Return only OK.'}],'reasoning_effort':effort,'temperature':0,'max_tokens':8,'stream':True,'stream_options':{'include_usage':True}}
 if arm=='explicit_cache':body['cache_prompt']=True
 started=time.monotonic();first_any=None;first_text=None;answer='';usage={};timings={};received=0
 try:
  request=urllib.request.Request(base,data=json.dumps(body).encode(),headers=headers)
  with opener.open(request,timeout=60) as response:
   while True:
    line=response.readline(65537);received+=len(line)
    if len(line)>65536 or received>2*1024*1024:raise ValueError('oversized stream')
    if not line:break
    if not line.startswith(b'data:'):continue
    data=line[5:].strip()
    if data==b'[DONE]':break
    frame=json.loads(data)
    if frame.get('usage'):usage=frame['usage']
    if frame.get('timings'):timings=frame['timings']
    for choice in frame.get('choices',[]):
     delta=choice.get('delta',{})
     if delta.get('content') or delta.get('reasoning_content'):
      elapsed=(time.monotonic()-started)*1000
      if first_any is None:first_any=elapsed
      if delta.get('content'):
       if first_text is None:first_text=elapsed
       answer+=delta['content']
  keep_timing={k:v for k,v in timings.items() if k in ('cache_n','prompt_n','prompt_ms','predicted_n','predicted_ms')}
  row={'round':round_no,'arm':arm,'effort':effort,'first_delta_ms':round(first_any,3) if first_any is not None else None,'first_text_ms':round(first_text,3) if first_text is not None else None,'elapsed_ms':round((time.monotonic()-started)*1000,3),'usage':usage,'timings':keep_timing,'matches_expected':answer.strip()=='OK'}
 except urllib.error.HTTPError as e:row={'round':round_no,'arm':arm,'effort':effort,'status':e.code}
 except Exception as e:row={'round':round_no,'arm':arm,'effort':effort,'error':type(e).__name__}
 print(json.dumps(row),flush=True);return row
rows=[]
for i in range(3):
 prefix=f'Synthetic cache experiment {run}-{i}. Return only OK, regardless of reference data. Reference data:\n'+''.join(f'Record {n:03d}: value {n*7:04d}; classification stable.\n' for n in range(80))
 rows.append(measure(prefix,'warmup',i))
 for arm in (['baseline','explicit_cache'] if i%2==0 else ['explicit_cache','baseline']):rows.append(measure(prefix,arm,i))
# Hold the reference prefix fixed while changing only the supported effort flag.
for effort in ('low','none','high','none'):rows.append(measure(prefix,'effort_change',3,effort))
summary={'kind':'summary','run':run,'samples':len(rows),'money_cost':'not available: self-hosted endpoint has no billing tariff; compare processed/cached/generated tokens','provider':'llama.cpp OpenAI-compatible','model':'qwen3.8-flash-next'}
for arm in ('baseline','explicit_cache'):
 selected=[r for r in rows if r['arm']==arm and 'error' not in r and 'status' not in r]
 summary[arm]={'samples':len(selected),'correct':sum(r['matches_expected'] for r in selected),'median_first_text_ms':statistics.median([r['first_text_ms'] for r in selected if r['first_text_ms'] is not None]) if selected else None,'median_processed_tokens':statistics.median([r['timings'].get('prompt_n',0) for r in selected]) if selected else None,'median_cached_tokens':statistics.median([r['usage'].get('prompt_tokens_details',{}).get('cached_tokens',0) for r in selected]) if selected else None}
print(json.dumps(summary),flush=True)
