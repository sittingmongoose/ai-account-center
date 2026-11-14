#!/usr/bin/env python3
"""Review regressions: real files, no IO injection, explicit delegation expectations."""
import base64
import importlib.util
import json
import pathlib
import sys
import tempfile
ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0,str(ROOT/'scripts/account-usage'))
import signin_expiry as helper
NOW=1800000000123
path=ROOT/'tests/spec/agent/signin-expiry.json'
table=json.loads(path.read_text())
table['cases']=[r for r in table['cases'] if not r['source'].startswith('review-')]
inputs=[
 ('review-M1-surrogate',r'{"accessToken":"\ud800","expiresAt":1800003600123}',True),
 ('review-M1-overflow',r'{"accessToken":"fixture","expiresAt":1800003600123,"extra":1e999}',True),
 ('review-M1-depth','{"accessToken":"fixture","expiresAt":1800003600123,"extra":'+'['*140+'0'+']'*140+'}',True),
 ('review-M2-unicode-digits',json.dumps({'accessToken':'fixture','expiresAt':'١٨٠٠٠٠٣٦٠٠١٢٣'}),True),
 ('review-M1-fraction', '{"accessToken":"fixture","expiresAt":1800003600123.5}',True),
 ('review-size-credential',' '*65537,True),
 ('review-size-settings',' '*1048577,True),
 ('review-M3-settings-cap',None,False),
 ('review-M3-settings-bom',None,False),
]
for name,body,fallback in inputs:
 with tempfile.TemporaryDirectory(prefix='aac-review-') as tmp:
  home=pathlib.Path(tmp); files=[]
  if name.startswith('review-M3'):
   (home/'custom').mkdir()
   body=json.dumps({'providerInstances':{'fixture':{'driver':'claudeAgent','config':{'homePath':str(home/'custom')}}},'padding':'x'*70000})
   if name.endswith('bom'): body='\ufeff'+body
   rel='.t3/userdata/settings.json'
   files.append({'path':str(home/'custom'),'directory':True,'mode':448})
  else: rel='.t3/userdata/settings.json' if name=='review-size-settings' else '.claude/.credentials.json'
  p=home/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(body)
  files.append({'path':str(p),'mode':384,**({'repeatSpaces':len(body)} if body.isspace() else {'text':body})})
  result=helper.collect(home,NOW,include_keychain=False)
  row={'source':name,'verb':'signin-expiry','args':{'home':tmp,'ccsDir':tmp+'/.ccs','nowMs':NOW,'platform':'linux','environment':{},'includeKeychain':False},'production':True,'files':files,'expected':result,'fallback':fallback}
  table['cases'].append(json.loads(json.dumps(row).replace(tmp,'${FIXTURE_999}')))
# Document the conservative grammar, independently of Rust's result.
def proven(raw):
 try:
  def integer(s):
   n=int(s)
   if abs(n)>9007199254740991:raise ValueError()
   return n
  def unsupported(s):raise ValueError()
  value=json.loads(raw,parse_int=integer,parse_float=unsupported,parse_constant=unsupported)
  def valid(v,depth=0):
   if depth>=128:return False
   if isinstance(v,str):return all(32<=ord(c)<=126 or c in '\t\n\r\v\f' for c in v)
   if isinstance(v,list):return all(valid(x,depth+1) for x in v)
   if isinstance(v,dict):return all(valid(k,depth+1) and valid(x,depth+1) and (not isinstance(x,str) or k not in ['expiresAt','refreshTokenExpiresAt'] or all(c in '0123456789 \t\r\n' for c in x)) for k,x in v.items())
   return True
  return valid(value)
 except (ValueError,RecursionError):return False
for row in table['cases']:
 if row['source'].startswith('review-'):continue
 row.pop('fallback',None)
 if not proven(json.dumps(row['args'])): row['fallback']=True
 for file in row['files']:
  if not all(32<=ord(c)<=126 or c in '\t\n\r\v\f' for c in file['path']): row['fallback']=True
  if file['path'].endswith('/.t3/userdata/settings.json') and 'text' in file and not proven(file['text'].lstrip('\ufeff')): row['fallback']=True
 for raw in row['args'].get('io',{}).get('reads',{}).values():
  if raw is not None and not proven(base64.b64decode(raw)):
   row['fallback']=True
 for item in row['args'].get('keychain',{}).values():
  if item and (item.get('status')=='oversized' or (item.get('status')=='payload' and not proven(item['text']))): row['fallback']=True
path.write_text(json.dumps(table,indent=2,ensure_ascii=True)+'\n')
print(len(table['cases']),'sign-in cases')
