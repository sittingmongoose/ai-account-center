#!/usr/bin/env python3
"""Review regressions: same synthetic queue for Python and the complete quota verb."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from unittest import mock
spec=importlib.util.spec_from_file_location('quota_parity',Path(__file__).with_name('quota.py'))
q=importlib.util.module_from_spec(spec);spec.loader.exec_module(q)
TABLE=json.loads(Path(__file__).with_name('quota-review.json').read_text())
FALLBACK={'fallback':'python'}

def main():
 binary=str(Path(sys.argv[1]).resolve());baseline='--baseline' in sys.argv
 server=q.http.server.ThreadingHTTPServer(('127.0.0.1',0),q.Handler)
 threading.Thread(target=server.serve_forever,daemon=True).start()
 endpoint='http://127.0.0.1:'+str(server.server_port);wrong=[]
 try:
  with tempfile.TemporaryDirectory(prefix='aac-quota-review-') as directory:
   home=Path(directory);auth=home/'.local/share/opencode/auth.json';auth.parent.mkdir(parents=True)
   for row in TABLE['cases']:
    provider=row['provider'];alias={'qwen':'alibaba-token-plan'}.get(provider,provider)
    key=json.dumps({'cookie':'fixture=made-up'}) if provider=='qwen' else 'fixture-dummy-token'
    auth.write_text(json.dumps({alias:{'type':'api','key':key}}))
    reply=q.response(row['payload'])
    if row.get('stream'):
     assert row['stream']=={'chunks':6,'intervalMs':1000}
     reply=('stream',{},reply[2])
    if row.get('disconnect'):reply=('disconnect',{},b'')
    replies=[reply]+([q.response(row['packs'])] if provider=='zai' else [])
    if provider=='qwen':replies=[q.response({'data':{'secToken':'fixture-sec'}}),reply]+[(500,{},b'private')]*3
    q.Handler.queue=replies.copy();q.Handler.calls=[]
    client=q.common.HttpClient();client.opener=q.LocalOpener(client.opener,endpoint)
    with mock.patch.dict(os.environ,{},clear=True),mock.patch.object(q.common,'utc_now',return_value=q.STAMP),mock.patch.object(q.usage,'utc_now',return_value=q.STAMP):
     reference=q.usage.collect(provider,'ubuntu',home=home,client=client)
    if row.get('referenceStatus'):assert reference['status']==row['referenceStatus'],row['name']
    q.Handler.queue=replies.copy();q.Handler.calls=[]
    request={'protocol':1,'verb':'quota-read','id':'quota-review','deadlineMs':15000,'args':{'provider':provider,'platform':'ubuntu','home':str(home),'endpoint':endpoint,'now':q.NOW}}
    process=subprocess.run([binary],input=json.dumps(request)+'\n',text=True,capture_output=True,timeout=20,env={'PATH':os.defpath,'TOKIO_WORKER_THREADS':'1'})
    event=json.loads(process.stdout)
    assert process.returncode==0 and event.get('type')=='result' and event.get('id')=='quota-review' and event.get('protocol')==1,row['name']
    actual=event['value'];target=FALLBACK if row['expect']=='fallback' else reference
    leaked='fixture-dummy-token' in process.stdout+process.stderr
    if actual!=target or leaked:
     wrong.append(row['name']);print('BASELINE RED:' if baseline else 'FAIL:',row['name'],'credential leak' if leaked else 'wrong result',flush=True)
    else:print('PASS:',row['name'],flush=True)
  # Big review L1 (Sol): plan credentials go through the shared no-follow descriptor read. A link
  # (the file itself or a folder below the home) or a FIFO is unproved: the whole verb delegates,
  # and the usage service is never contacted for it.
  for label in ('final-link','ancestor-link','fifo'):
   with tempfile.TemporaryDirectory(prefix='aac-quota-link-') as directory:
    home=Path(directory);real=home/'outside/auth.json';real.parent.mkdir()
    real.write_text(json.dumps({'kimi-code':{'type':'api','key':'fixture-dummy-token'}}))
    auth=home/'.local/share/opencode/auth.json'
    if label=='ancestor-link':
     (home/'.local/share').mkdir(parents=True);(home/'.local/share/opencode').symlink_to(real.parent)
    else:
     auth.parent.mkdir(parents=True)
     if label=='final-link':auth.symlink_to(real)
     else:os.mkfifo(auth)
    q.Handler.queue=[q.response({'usage':{'used':3,'limit':2048,'resetTime':q.STAMP}})];q.Handler.calls=[]
    request={'protocol':1,'verb':'quota-read','id':'quota-review','deadlineMs':15000,'args':{'provider':'kimi-code','platform':'ubuntu','home':str(home),'endpoint':endpoint,'now':q.NOW}}
    process=subprocess.run([binary],input=json.dumps(request)+'\n',text=True,capture_output=True,timeout=20,env={'PATH':os.defpath,'TOKIO_WORKER_THREADS':'1'})
    event=json.loads(process.stdout)
    name='L1-credential-'+label
    if process.returncode!=0 or event.get('value')!=FALLBACK or q.Handler.calls:
     wrong.append(name);print('BASELINE RED:' if baseline else 'FAIL:',name,'answered without delegating',flush=True)
    else:print('PASS:',name,flush=True)
 finally:server.shutdown();server.server_close()
 if wrong:raise SystemExit(f'{len(wrong)} regression cases failed')
 if baseline:raise SystemExit('Baseline unexpectedly passed')
 print(f'quota review parity: {len(TABLE["cases"])} table cases + 3 credential-link cases pass, no credential text published')
if __name__=='__main__':main()
