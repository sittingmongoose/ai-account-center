"""Offline Python oracle; fixture inputs contain only synthetic accounts."""
import contextlib, copy, datetime as dt, importlib.machinery, importlib.util, io, json
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[3]
loader=importlib.machinery.SourceFileLoader('aac_oracle',str(ROOT/'scripts/agent-usage/aac-usage'))
spec=importlib.util.spec_from_loader(loader.name,loader);m=importlib.util.module_from_spec(spec);loader.exec_module(m)
now=dt.datetime(2027,1,15,8,tzinfo=dt.timezone.utc)
base={'generatedAt':'2027-01-15T08:00:00Z','staleAfterSeconds':300,'accounts':[{'provider':'codex','providerLabel':'Codex','label':'Work','profile':'work','status':'ok','host':'ubuntu','sampledAt':'2027-01-15T07:59:00Z','windows':[]}]}
cases=[]
def case(doc,args=(),history=None,signins=None):
    out=io.StringIO()
    with patch.object(m,'load',lambda folder,name:copy.deepcopy(history if name=='history.json' and history is not None else doc)),patch.object(m,'signins_doc',lambda folder:copy.deepcopy(signins)),patch.object(m,'now_utc',lambda:now),contextlib.redirect_stdout(out):m.main(['--dir','/snapshot',*args])
    cases.append({'snapshot':doc,'args':list(args),'expected':m.ascii(out.getvalue()),'history':history,'signins':signins})
doc=copy.deepcopy(base);doc['accounts'][0]['label']='Café 𐀀';case(doc);case(doc,('--json',))
case(base);case(base,('--provider','anthropic'));case(base,('--json',))
for status in ['cached','unavailable','error','needs_sign_in']:
    for reason in ['rate_limited','read_failed','refresh_pending','source_changed','PRIVATE_ERROR'] if status=='cached' else ['']:
        doc=copy.deepcopy(base);doc['accounts'][0].update(status=status,cacheReason=reason);case(doc)
windows=[{'used':0.125}, {}, {'usedPercent':70,'remainingPercent':30}, {'unlimited':True}, {'remaining':40,'limit':100,'unit':'USD'}, {'used':5.5,'unit':'tokens'}, {'cached':True}, {'resetAt':'2027-01-15T07:59:30Z'}, {'resetAt':'2027-01-15T09:00:00Z'}, {'expiresAt':'2027-01-16T08:00:00Z'}, {'cached':True,'sampledAt':'2027-01-15T07:58:00Z'}]
for w in windows:
    doc=copy.deepcopy(base);doc['accounts'][0]['windows']=[dict(key='weekly',label='Weekly',pool='team',**w)];case(doc);case(doc,('--json',))
for pace,minutes,full in [(30,5,'2027-01-15T08:10:00Z'),(-2.5,60,None),(0,90,None),(40,5,'2027-01-15T07:59:00Z')]:
    for used in [50,100]:
        doc=copy.deepcopy(base);doc['accounts'][0]['windows']=[{'key':'weekly','usedPercent':used,'burn':{'pace':pace,'paceMinutes':minutes,'lastHour':30,'projectedFullAt':full,'runsOutBeforeReset':True}}];case(doc)
for state in ['ok','warn','critical','unavailable','unsupported','logged_out']:
    for extra in [{},{'loggedOutAt':'2027-01-15T09:00:00Z','accessExpiresAt':'2027-01-15T09:00:00Z'},{'loggedOutAt':'2027-01-15T07:00:00Z'},{'refreshExpiresAt':'2027-01-15T07:00:00Z'},{'accessExpiresAt':'2027-01-15T09:00:00Z'}]:
        doc=copy.deepcopy(base);doc['signIn']=[dict(provider='codex',name='work',machine='ubuntu',state=state,fixCommand='codex login',**extra)];case(doc)
for args in [('history',),('history','--json'),('history','--provider','openai'),('history','--window','weekly'),('history','--hours','1'),('history','--account','work')]:
    history={'version':1,'generatedAt':base['generatedAt'],'series':[{'provider':'codex','label':'Work','accountId':'codex:work','window':'weekly','windowLabel':'Weekly','points':[['2027-01-15T07:59:00Z',12.5,None],['2026-01-01T00:00:00Z',30,70],['invalid',40,None]]},{'provider':'claude','hidden':True,'points':[]}]}
    case(base,args,history=history)
for args in [('signins',),('signins','--json'),('signins','--account','nobody')]:
    signins={'version':1,'runner':'ubuntu','generatedAt':'2027-01-15T06:00:00Z','staleAfterSeconds':3600,'renewWithinHours':12,'needsYou':[{'how':'Sign in once'}],'signIns':[{'host':'ubuntu','profile':'work','email':'test@example.invalid','state':'ok','signInEndsAt':'2027-01-16T08:00:00Z','lastResult':'renewed_ok'}]}
    case(base,args,signins=signins)
open(ROOT/'crates/aac-snapshot/tests/fixtures/reader-corpus.json','w').write(json.dumps(cases,indent=2)+'\n')
print(len(cases),'oracle cases')
