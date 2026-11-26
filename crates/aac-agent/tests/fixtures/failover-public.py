"""Regenerate the native public-boundary oracle; synthetic values, no disk/network reads."""
import copy
import importlib.util
import json
from pathlib import Path
ROOT = Path(__file__).resolve().parents[4]
spec = importlib.util.spec_from_file_location('failover_usage', ROOT / 'scripts/account-usage/failover_usage.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
cases = []
def add(kind, value, **kwargs):
    fn = {'candidate': m.public_candidate, 'sample': m.public_sample, 'changed': m._changed_after_collection, 'unverified': m._unverified_after_collection}[kind]
    cases.append(dict(kind=kind, value=value, args=kwargs, expected=fn(value, **kwargs)))
c = dict(locator='a'*64, selectors=['discover','discover'], identity=dict(kind='codex-principal-v1', key='b'*64), revision='c'*64, email='fixture@example.test', plan='Plus')
for provider in m.KINDS:
    for identity in m.KINDS[provider]:
        v=copy.deepcopy(c);v['identity']['kind']=identity
        add('candidate', v, provider=provider)
        add('candidate', v, provider=provider, request=True)
for key,values in dict(locator=[None,'a'*63,'A'*64],revision=[False,'secret'],selectors=[[],['*'],['discover']*33,['x:y','x:y']],identity=[{},dict(kind='zai-credential',key='b'*64)],email=['Bearer private','bad',None],plan=['https://private.test','sk_secret',None,' Max ']).items():
    for value in values:
        v=copy.deepcopy(c);v[key]=value
        for request in (False,True):add('candidate',v,provider='codex',request=request)
v=copy.deepcopy(c);v['_token']='private';add('candidate',v,provider='codex');add('candidate',v,provider='codex',request=True)
s=dict(provider='codex',platform='ubuntu',status='ok',email='fixture@example.test',plan='Plus',sampledAt='2001-01-01T01:00:00Z',fetchedAt='2001-01-01T01:00:01Z',windows=[dict(key='five_hour',label='5-hour usage',usedPercent=0)])
for provider in m.KINDS:
    for platform in ('ubuntu','mac','windows'):
        v=copy.deepcopy(s);v.update(provider='opencode-go' if provider=='opencode-wallet' else provider,platform=platform)
        add('sample',v,provider=provider,platform=platform)
for key,values in dict(status=['unavailable','cached'],sampledAt=[None,'2100-01-01T00:00:00Z','2001-01-02T00:00:00Z','2001-01-01T00:00:00+02:00'],plan=['https://private.test'],email=['bad'],windows=[[],[dict(key='x',label='Usage')],[dict(key='expires',label='Subscription',expiresAt='2002-01-01T00:00:00Z')]]).items():
    for value in values:
        v=copy.deepcopy(s);v[key]=value;add('sample',v,provider='codex',platform='ubuntu')
for key,values in dict(usedPercent=[True,-1,150],remainingPercent=[101,0],remaining=[-10],status=['unavailable','cached'],sampledAt=['2100-01-01T00:00:00Z'],kind=['private','balance'],clears=[['five_hour'],['x','x'],['*']],unit=['sk_private'],unlimited=[True,'yes'],poolId=['private'],modelIds=[['good','bad secret','https://private.test']]).items():
    for value in values:
        v=copy.deepcopy(s);v['windows'][0][key]=value;add('sample',v,provider='codex',platform='ubuntu')
for code in [None,'rate_limited','source_changed','team_mismatch','invalid_response','made_up']:
    for retry in [None,0,123,999999,-1,True]:
        for flag in [False,True]:
            v=dict(failureCode=code,retryAfterSeconds=retry,rateLimited=flag,sample=s)
            add('changed',v);add('unverified',v)
print(json.dumps(cases,sort_keys=True,indent=2))
