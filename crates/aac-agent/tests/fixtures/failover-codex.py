"""Regenerate synthetic Codex normalization cases from the Python oracle."""
import sys,json,copy
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[4]/'scripts/account-usage'))
import failover_core as m
now=1795716000
payloads=[{}, {'rate_limit':{'primary_window':{'used_percent':0,'limit_window_seconds':604800,'reset_at':1795800000}}}, {'credits':{'balance':'0','unlimited':False,'has_credits':False},'rate_limit_reset_credits':{'available_count':0}}, {'rateLimit':{'primaryWindow':{'usedPercent':150,'limitWindowSeconds':120,'resetAfterSeconds':0}}}]
for used in [None,False,-1,0,12.12345,100,150,'10']:
 for reset in [None,0,1795800000]:
  payloads.append({'rate_limit':{'primary_window':{'used_percent':used,'reset_at':reset,'reset_after_seconds':60}}})
for credits in [{'balance':-1,'unlimited':False},{'balance':'12.50'},{'balance':True},{'unlimited':True},{'balance':0,'has_credits':False}]:payloads.append({'credits':credits})
for monthly in [{'used':0,'limit':10,'remaining_percent':100,'reset_at':1795800000},{'remaining_percent':0},{'remaining_percent':101},{'used':-1,'limit':-3},{'used':20,'limit':10}]:payloads.append({'spend_control':{'individual_limit':monthly}})
payloads.append({'additional_rate_limits':[{'limit_name':'Extra','rate_limit':{'primary_window':{'used_percent':5}}}]*20,'code_review_rate_limit':{'secondary_window':{'used_percent':0}}})
print(json.dumps([dict(payload=p,now=now,expected=m._codex_windows(p,now))for p in payloads],indent=2,sort_keys=True))
