#!/usr/bin/env python3
"""Property corpus: every frozen whitespace/control point x field x position."""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
ROOT=pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0,str(ROOT/'scripts/account-usage'))
import signin_expiry as helper
spec=importlib.util.spec_from_file_location('parity',pathlib.Path(__file__).with_name('run.py'))
parity=importlib.util.module_from_spec(spec);spec.loader.exec_module(parity)
TABLE=json.loads(pathlib.Path(__file__).with_name('whitespace-controls.json').read_text())
FALLBACK={'fallback':'python'}
NOW=1800000000123

def insert(value,point,position):
 at={'start':0,'middle':len(value)//2,'end':len(value)}[position]
 return value[:at]+chr(point)+value[at:]

def inventory():
 rows=[];meta=[]
 for platform in TABLE['platforms']:
  for field in TABLE['inventoryFields']:
   for location in TABLE['inventoryLocations']:
    for point in TABLE['codePoints']:
     for position in TABLE['positions']:
      key=insert(field,point,position) if location=='key' else field
      value={'cpu':'1 2 3 4','mount':'/fixture'}.get(field,'16')
      if location=='value':value=insert(value,point,position)
      rows.append({'platform':platform,'probe':'cores=4\nmemTotalKb=100\nmemAvailKb=50\ndiskTotalKb=100\ndiskFreeKb=50\n'+key+'='+value+'\n'})
      meta.append([platform,field,location,point,position])
 return rows,meta

def agent(binary,verb,args):
 result=subprocess.run([binary],input=json.dumps({'protocol':1,'verb':verb,'id':'parity','deadlineMs':1000,'args':args})+'\n',text=True,capture_output=True,timeout=5,env={**os.environ,'TOKIO_WORKER_THREADS':'1'})
 return parity.terminal(result)

def main():
 if sys.argv[1]=='capture':
  rows,_=inventory();pathlib.Path(sys.argv[2]).write_text(json.dumps(rows));return
 binary=str(pathlib.Path(sys.argv[1]).resolve());wrong=[];counts={'equal':0,'fallback':0};witness='--baseline' in sys.argv
 if '--signin-only' not in sys.argv:
  rows,meta=inventory();refs=json.loads(pathlib.Path(sys.argv[2]).read_text())
  assert len(rows)==len(refs)
  if witness:
   selected=[i for i,m in enumerate(meta) if m[1] in ['load','cpu'] and m[3]==0x85]
   rows=[rows[i] for i in selected];meta=[meta[i] for i in selected];refs=[refs[i] for i in selected]
  if witness:
   actuals=[agent(binary,'inventory',r) for r in rows]
  else:
   actuals=[]
   for at in range(0,len(rows),256):
    values=agent(binary,'inventory',{'probes':rows[at:at+256]})
    assert isinstance(values,list) and len(values)==len(rows[at:at+256])
    actuals.extend(values)
  for m,expected,actual in zip(meta,refs,actuals):
   if actual==FALLBACK:counts['fallback']+=1
   elif actual==expected:counts['equal']+=1
   else:wrong.append({'case':m,'reference':expected,'agent':actual})
  print('inventory property',len(rows),'cases',counts,'wrong',len(wrong),flush=True)
 counts={'equal':0,'fallback':0};before=len(wrong);signin_count=0
 with tempfile.TemporaryDirectory(prefix='aac-property-') as tmp:
  home=pathlib.Path(tmp);(home/'custom').mkdir();(home/'.claude').mkdir();(home/'.t3/userdata').mkdir(parents=True)
  for field in TABLE['signinFields']:
   if witness and field!='settings.homePath':continue
   if '--production' in sys.argv and field.startswith('environment.'):continue
   for point in TABLE['codePoints']:
    if witness and point not in range(0x1c,0x20):continue
    for position in TABLE['positions']:
     signin_count+=1
     credentials={'accessToken':'fixture','refreshToken':'fixture','expiresAt':NOW+3600000,'refreshTokenExpiresAt':NOW+7200000}
     settings={'providerInstances':{'fixture':{'driver':'claudeAgent','config':{'homePath':str(home/'custom')}}}}
     args={'home':tmp,'ccsDir':tmp+'/.ccs','nowMs':NOW};environment={};extra={};reference_error=None
     mutate=lambda s:insert(s,point,position)
     config=settings['providerInstances']['fixture']
     if field=='settings.homePath':config['config']['homePath']=mutate(config['config']['homePath'])
     elif field=='settings.key':config['config']={mutate('homePath'):str(home/'custom')}
     elif field=='settings.driver':config['driver']=mutate('claudeAgent')
     elif field.startswith('credential.'):
      key=field.split('.')[1]
      if key=='key':credentials[mutate('expiresAt')]=credentials.pop('expiresAt')
      else:credentials[key]=mutate(str(credentials[key]))
     elif field.startswith('args.'):args[field.split('.')[1]]=mutate(args[field.split('.')[1]])
     else:environment[field.split('.')[1]]=mutate(str(home/'custom'));extra={'platform':'linux','environment':environment,'includeKeychain':True,'keychain':{}}
     (home/'.claude/.credentials.json').write_text(json.dumps(credentials))
     (home/'.t3/userdata/settings.json').write_text(json.dumps(settings))
     try:
      with parity.mock.patch.dict(os.environ,environment,clear=True):
       expected=helper.collect(pathlib.Path(args['home']),NOW,include_keychain=bool(extra),ccs_dir=pathlib.Path(args['ccsDir']))
     except (ValueError,OSError) as error:reference_error=type(error).__name__;expected=None
     actual=agent(binary,'signin-expiry',args|extra)
     if actual==FALLBACK:counts['fallback']+=1
     elif reference_error is None and actual==expected:counts['equal']+=1
     else:wrong.append({'case':[field,point,position],'reference':reference_error or expected,'agent':actual})
 print('signin property',signin_count,'cases',counts,'wrong',len(wrong)-before,flush=True)
 for row in wrong[:8]:print(json.dumps(row),flush=True)
 if wrong:raise SystemExit(f'{len(wrong)} wrong ordinary answers'+(' at baseline (expected red)' if witness else ''))
 if witness:raise SystemExit('baseline unexpectedly passed')
if __name__=='__main__':main()
