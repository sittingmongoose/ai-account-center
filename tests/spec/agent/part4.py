#!/usr/bin/env python3
"""M2 inventory parity: unchanged Python on real fixture homes, no provider access.

Each row declares equality or fixed fallback in advance. No fields, order, causes,
identity/revision hashes or nulls are normalized. Production uses the same suite.
"""
import base64
import copy
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
import failover_usage as helper
from run import terminal
from part3 import image, load

core_tests = load('m2p4_core_fixtures', 'tests/unit/account-usage/failover_core_test.py')


def write(home, path, value, mode=0o600):
    p = home / path
    p.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    p.write_text(json.dumps(value), encoding='utf-8')
    p.chmod(mode)
    return p


def codex(home, **kwargs):
    return write(home, '.codex/auth.json', core_tests.codex_auth(**kwargs))


def claude(home):
    core_tests.claude_files(home)


def plan(home, provider='zai', secret='fixture-key-1234'):
    return write(home, '.local/share/opencode/auth.json', {provider: {'type': 'api', 'key': secret}})


def muse(home, token='dca:synthetic-muse', address='one@example.test'):
    return write(home, '.config/muse/auth.json', {'providers': {'meta': {'access_token': token, 'user_email': address}}})


def key(home, provider='zai', slot='1234abcd', **kwargs):
    record = dict(version=1, provider=provider, keyId=slot, secret='fixture-saved-key-1234')
    record.update(kwargs)
    p = write(home, f'scope/account-usage/keys/{provider}-{slot}.json', record)
    p.parent.chmod(0o700)
    return p


def cases():
    rows = []
    def add(name, provider, setup=lambda h: None, fallback=False, request=None, nonempty=False):
        rows.append((name, provider, setup, fallback, request or {}, nonempty))
    for provider in helper.KINDS:
        add('empty-' + provider, provider, fallback=provider in {'antigravity', 'opencode-wallet', 'cursor'})
    add('codex-default', 'codex', codex, nonempty=True)
    add('claude-default', 'claude', claude, nonempty=True)
    add('muse-default', 'muse', muse, nonempty=True)
    for provider in ['kimi-code', 'zai', 'opencode-go']:
        add('opencode-' + provider, provider, lambda h, p=provider: plan(h, p), nonempty=True)
        add('saved-key-' + provider, provider, lambda h, p=provider: key(h, p), nonempty=True)
    def core_roots(home, provider):
        roots = [home / ('scope/codex-instances/saved' if provider == 'codex' else 'scope/instances/saved'),
                 home / ('.codex-t3/slot' if provider == 'codex' else '.claude-t3/slot'), home / 'configured']
        for root in roots:
            if provider == 'codex': write(home, str(root.relative_to(home) / 'auth.json'), core_tests.codex_auth())
            else: core_tests.claude_files(home, root)
        write(home, '.t3/userdata/settings.json', {'providerInstances': {
            'a': {'driver': 'codex' if provider == 'codex' else 'claudeAgent', 'config': {'homePath': str(roots[-1])}},
            'b': {'driver': 'codex' if provider == 'codex' else 'claudeAgent', 'config': {'homePath': str(roots[-1])}}}})
    for p in ['codex', 'claude']:
        add('saved-t3-settings-merge-' + p, p, lambda h, p=p: core_roots(h, p), nonempty=True)
    def muse_slots(h):
        muse(h)
        write(h, 'scope/muse-homes/abcd1234/muse/auth.json', {'providers': {'meta': {'access_token':'dca:second-fixture','user_email':'two@example.test'}}})
    add('muse-saved-slot', 'muse', muse_slots, nonempty=True)
    for n in [0, 1, 511, 512, 513]:
        add('codex-id-bound-' + str(n), 'codex', lambda h, n=n: codex(h, claims={'chatgpt_user_id':'u'*n}))
    for value in ['fixture-wrong', '', None, 0, False]:
        add('codex-account-' + repr(value), 'codex', lambda h, v=value: codex(h, account_id=v))
    for value in ['two@example.test', 'bad', ' a@b.test ', '\ta@b.test', 'a@b.test\t', 'a\tb@c.test', 'bearer a@b.test', 'a@b.test' + 'x'*254]:
        add('codex-email-' + repr(value), 'codex', lambda h, v=value: codex(h, payload={'email':v}))
    for n in [63, 64, 65, 160, 161]:
        add('plan-bound-' + str(n), 'codex', lambda h, n=n: codex(h, claims={'chatgpt_plan_type':'p'*n}), nonempty=True)
    for value in [' max ', '\tmax', 'max\t', 'ma\tx', 'sk-x', 'sk_x', 'https://example.test', 'access_token', 'dca:fake', 'Bearer x', 'eyJ12345678']:
        add('public-plan-' + repr(value), 'codex', lambda h, v=value: codex(h, claims={'chatgpt_plan_type':v}), nonempty=True)
    for value in ['', ' ', '\tfixture\t', 'bad token', 'x'*16384, 'x'*16385]:
        add('codex-secret-' + str(len(value)) + repr(value[:8]), 'codex', lambda h, v=value: codex(h, access_token=v))
    # All C0/DEL values on the raw display field: proof allowlist and reference text
    # check have different jobs. Shared whitespace must not bypass the latter.
    for cp in list(range(32)) + [127, 133, 0x200e, 0x2028, 0xfeff]:
        add('text-control-' + str(cp), 'codex', lambda h, cp=cp: codex(h, claims={'chatgpt_plan_type':'pro'+chr(cp)+'plan'}), fallback=cp not in [9,10,11,12,13])
    for raw in ['{"tokens":', '[1]', '{"tokens":NaN}', '{"unknown":1.5}', '{"unknown":1e0}', '{"unknown":9007199254740992}', '{"unknown":"\\ud800"}', '{"tokens":{},"tokens":{}}']:
        add('json-grammar-' + raw[:20], 'codex', lambda h, raw=raw: write(h, '.codex/auth.json', {}).write_text(raw), fallback=True)
    for number in [False, True, 1, 0, -1, 9007199254740991]:
        add('number-field-' + str(number), 'codex', lambda h, n=number: codex(h, last_refresh=n), nonempty=True)
    for cap, provider, path, setup in [(1048576,'codex','.codex/auth.json',codex), (65536,'muse','.config/muse/auth.json',muse), (4096,'zai','scope/account-usage/keys/zai-1234abcd.json',key)]:
        for delta in [-1, 0, 1]:
            def capped(h, cap=cap, delta=delta, path=path, setup=setup):
                setup(h)
                p=h/path
                p.write_bytes(p.read_bytes() + b' '*(cap+delta-p.stat().st_size))
            add(f'file-cap-{provider}-{delta}',provider,capped,fallback=delta>0,nonempty=delta<=0)
    def bom(h):
        codex(h); p=h/'.codex/auth.json'; p.write_bytes(b'\xef\xbb\xbf'+p.read_bytes())
    add('core-bom', 'codex', bom, nonempty=True)
    for path in ['relative', '', '/'+ 'x'*4096, '/fixture/\tbad', '/fixture/\nbad']:
        add('scope-bound-' + repr(path[:20]), 'codex', fallback=True, request={'homeDir':path})
    for value in [{}, {'id':'ok','email':'a@b.test','profilePath':'/x\ty'}, {'id':'a'*65,'email':'a@b.test'}, {'id':'ok','email':'a@b.test','isDefault':1}]:
        add('claude-profile-bound-' + str(len(rows)), 'claude', fallback=True, request={'claudeProfile':value})
    def symlink(h):
        codex(h); (h/'.codex/auth.json').rename(h/'.codex/real'); (h/'.codex/auth.json').symlink_to('real')
    add('credential-link', 'codex', symlink, fallback=True)
    def directory_file(h): (h/'.codex/auth.json').mkdir(parents=True)
    add('non-regular-credential', 'codex', directory_file, fallback=True)
    def fifo(h):
        (h/'.codex').mkdir(); os.mkfifo(h/'.codex/auth.json')
    add('fifo-credential', 'codex', fifo, fallback=True)
    for field, value in [('providerInstances', []), ('providerInstances', 1)]:
        add('settings-shape-' + repr(value), 'codex', lambda h, f=field, v=value: write(h,'.t3/userdata/settings.json',{f:v}), fallback=True)
    for value in ['relative', 'x'*1025, '\0bad', 0, [], {}]:
        add('settings-path-' + repr(value)[:20], 'codex', lambda h,v=value: write(h,'.t3/userdata/settings.json',{'providerInstances':{'one':{'driver':'codex','config':{'homePath':v}}}}), fallback=True)
    # REVIEW-M2-part4-cheap M1: pathlib keeps a leading // as its own root, so Python refuses
    # these homes; the agent must fall back, never return an extra t3 candidate.
    for prefix in ['/', '//']:
        for driver, provider in [('codex', 'codex'), ('claudeAgent', 'claude')]:
            add('settings-path-slashes-' + str(len(prefix) + 1) + '-' + provider, provider,
                lambda h, p=prefix, d=driver: write(h, '.t3/userdata/settings.json',
                    {'providerInstances': {'one': {'driver': d, 'config': {'homePath': p + str(h / '.codex-t3/x')}}}}),
                fallback=True)
    for count in [64, 65]:
        def homes(h,count=count):
            for i in range(count): (h/f'.codex-t3/slot{i:03}').mkdir(parents=True)
        add('homes-cap-' + str(count),'codex',homes,fallback=count>64)
    def omp(h):
        p=h/'.omp/agent/agent.db'; p.parent.mkdir(parents=True)
        with sqlite3.connect(p) as c: c.execute('CREATE TABLE auth_credentials (provider,disabled_cause,credential_type,data)')
    for provider in ['muse','zai','kimi-code','qwen','opencode-go','antigravity']:
        add('omp-' + provider,provider,omp,fallback=True)
    add('muse-capsule','muse',lambda h: (muse(h),write(h,'scope/account-usage/muse-console-session.json',{})),fallback=True)
    add('qwen-session','qwen',lambda h: plan(h,'alibaba-token-plan',json.dumps({'cookie':'sid=synthetic'})),nonempty=True)
    # REVIEW-M2-part4-cheap M2: Python reads Safari's cookie store for Qwen on every platform;
    # a present file (readable or not) must fall back, never look like a complete empty answer.
    safari = 'Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies'
    def safari_cookies(h, readable):
        p = h / safari; p.parent.mkdir(parents=True, mode=0o700)
        p.write_bytes(b'cook\x00\x00\x00\x00'); p.chmod(0o600 if readable else 0o000)
    add('qwen-safari-cookies', 'qwen', lambda h: safari_cookies(h, True), fallback=True)
    add('qwen-safari-unreadable', 'qwen', lambda h: safari_cookies(h, False), fallback=True)
    add('antigravity-native','antigravity',lambda h: write(h,'.gemini/antigravity-cli/antigravity-oauth-token',{'auth_method':'consumer','token':{'access_token':'synthetic'},'email':'one@example.test','subject':'123'}),fallback=True,nonempty=True)
    add('opencode-wallet','opencode-wallet',lambda h: write(h,'scope/account-usage/opencode-console-session.json',{}),fallback=True)
    for kwargs in [{'version':2},{'fingerprint':'wrong'},{'secret':'short'},{'unknown':'ignored?' }]:
        add('invalid-key-' + next(iter(kwargs)),'zai',lambda h,kw=kwargs:key(h,**kw),fallback=True)
    add('unsafe-key-mode','zai',lambda h:key(h).chmod(0o644),fallback=True)
    for value in [dict(cookie='b=two; a=one',gatewayCookie='b=gateway; a=one'), dict(cookie='sid=one',baseUrl='https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/'), dict(cookie='dup=1; dup=2'), dict(cookie='bad'), dict(cookie='a=b;'), dict(cookie='a=b\tc'), dict(cookie='a=b\n')]:
        add('qwen-cookie-' + str(len(rows)), 'qwen', lambda h, v=value:plan(h,'alibaba-token-plan',json.dumps(v)))
    for provider,variable in [('zai','ZAI_API_KEY'),('kimi-code','KIMI_API_KEY'),('opencode-go','OPENCODE_GO_API_KEY')]:
        add('environment-' + provider,provider,request={'testEnvironment':{variable:'fixture-environment-key'}},nonempty=True)
    add('xdg-muse','muse',lambda h:write(h,'alternate/muse/auth.json',{'providers':{'meta':{'access_token':'dca:fixture-xdg','user_email':'one@example.test'}}}),request={'testEnvironment':{'XDG_CONFIG_HOME':'${HOME}/alternate'}},nonempty=True)
    def saved_antigravity(h):
        import hashlib
        import failover_special as special
        raw=json.dumps({'auth_method':'consumer','token':{'access_token':'synthetic-saved'},'email':'one@example.test','subject':'123'},separators=(',',':')).encode()
        fingerprint=hashlib.sha256(raw).hexdigest()
        path=h/f'scope/antigravity-instances/saved/ubuntu/credential-{fingerprint}.bin'
        path.parent.mkdir(parents=True,mode=0o700); path.write_bytes(raw);path.chmod(0o600)
        identity=special._identity('one@example.test','123')
        write(h,'scope/antigravity-profiles/registry-000000000001.json',{'version':1,'revision':1,'profiles':[{'id':'saved','email':'one@example.test','identityKey':identity[1],'plan':'Pro','credential':{'fingerprint':fingerprint,'identity':{'email':'one@example.test','subject':'123','source':'provider-userinfo','verifiedAt':'2026-01-01T00:00:00Z'}}}]})
    add('antigravity-saved','antigravity',saved_antigravity,fallback=True,nonempty=True)
    def wallet(h):
        sys.path.insert(0,str(ROOT/'browser-bridge/opencode-muse/native-host'))
        import opencode_console as console
        p=write(h,'scope/account-usage/opencode-console-session.json',{'schemaVersion':1,'origin':console.ORIGIN,'source':'browser-native-cookie-api','workspaceId':'wrk_fixture','capturedAt':console.now_iso(),'cookies':[{'name':'auth','value':'synthetic-wallet','domain':'opencode.ai','path':'/','secure':True,'hostOnly':True}]})
        p.parent.chmod(0o700)
    add('wallet-valid-capsule','opencode-wallet',wallet,fallback=True,nonempty=True)
    for n in [7,8,512,513]:
        add('saved-secret-bound-' + str(n),'zai',lambda h,n=n:key(h,secret='k'*n),fallback=n in [7,513],nonempty=n in [8,512])
    for n in [32767,32768,32769]:
        add('plan-secret-bound-' + str(n),'zai',lambda h,n=n:plan(h,secret='k'*n),nonempty=n<=32768)
    for n in [1023,1024,1025]:
        def settings_bound(h,n=n):
            # A padded but valid settings home tests the raw pre-trim limit.
            codex(h)
            raw=str(h/'.codex');raw += ' '*(n-len(raw))
            write(h,'.t3/userdata/settings.json',{'providerInstances':{'one':{'driver':'codex','config':{'homePath':raw}}}})
        add('settings-text-bound-' + str(n),'codex',settings_bound,fallback=n>1024,nonempty=True)
    add('alternate-json-escape','codex',lambda h:write(h,'.codex/auth.json',{}).write_text('{"unused":"\\u0061"}'),fallback=True)
    # Big review L3 (Astra L3 / Sol L3): Python cuts the matching key names to 128 before the
    # json/dpapi twins collapse; 65 keys with stale twins are 64 slots there, never 65.
    def twinned_keys(h, count):
        for i in range(count):
            p = key(h, 'zai', slot=f'{i:08x}')
            twin = p.with_suffix('.dpapi'); twin.write_bytes(b'stale'); twin.chmod(0o600)
    for count in [64, 65]:
        add(f'key-twins-{count}', 'zai', lambda h, c=count: twinned_keys(h, c), nonempty=True)
    # Big review L4 (Astra L1 / Sol L4): production omits homeDir, so the environment's home must
    # pass the explicit rule: Python's Path.home() and abspath normalize these spellings to H.
    for label, spelling in [('canonical', '${HOME}'), ('inner-double', '${HOME_DOUBLE}'), ('trailing-dot', '${HOME}/.'),
                            ('trailing-slash', '${HOME}/'), ('dot-dot', '${HOME}/../${HOME_NAME}')]:
        add('default-home-' + label, 'codex', codex, request={'homeDir': '__omit__', 'testEnvironment': {'HOME': spelling}}, nonempty=True)
    add('default-home-double-root', 'codex', codex, fallback=True, request={'homeDir': '__omit__', 'testEnvironment': {'HOME': '/${HOME}'}})
    for value in ['non-ascii-\u00e9', 'surrogate-\udcff']:
        add('environment-unproven-' + str(len(rows)),'zai',fallback=True,request={'testEnvironment':{'ZAI_API_KEY':value}},nonempty=True)
    return rows


def call(binary, request, environment=None):
    result=subprocess.run([binary],input=json.dumps(dict(protocol=1,verb='failover-inventory',id='parity',deadlineMs=12000,args=request))+'\n',text=True,capture_output=True,timeout=18,
                          env={'PATH':os.environ.get('PATH','/usr/bin:/bin'),**(environment or {})})
    assert not result.stderr, 'unexpected diagnostics'
    return terminal(result)


def check(actual, expected, fallback):
    assert actual == ({'fallback':'python'} if fallback else expected), 'inventory differs from reference'


def main(binary=None):
    binary=str(Path(binary or sys.argv[1]).resolve())
    counts={'equal':0,'fallback':0}
    for name,provider,setup,fallback,extra,nonempty in cases():
        with tempfile.TemporaryDirectory(prefix='aac-m2p4-') as tmp:
            home=Path(tmp); setup(home)
            request=dict(version=1,action='inventory',provider=provider,homeDir=str(home),ccsDir=str(home/'scope'))
            request.update(extra)
            if request.get('homeDir') == '__omit__': del request['homeDir']
            spell={'${HOME_DOUBLE}':str(home.parent)+'//'+home.name,'${HOME_NAME}':home.name,'${HOME}':str(home)}
            def spelled(v):
                for token,text in spell.items(): v=v.replace(token,text)
                return v
            environment={k:spelled(v) for k,v in request.pop('testEnvironment',{}).items()}
            # Invalid scope rows can point outside the fixture but fail validation before IO.
            # Valid scope reads always remain in this newly created home.
            saved=image(home) if name not in ('fifo-credential', 'qwen-safari-unreadable') else None
            with mock.patch.dict(os.environ,environment,clear=True), mock.patch('urllib.request.OpenerDirector.open',side_effect=AssertionError('fixture network forbidden')):
                expected=helper.run(request)
            actual=call(binary,request,environment)
            try: check(actual,expected,fallback)
            except AssertionError as error: raise AssertionError(name+': '+str(error)) from None
            if nonempty: assert expected['candidates'], name+': empty reference witness'
            if saved is not None: assert image(home)==saved, name+': fixture changed'
            assert 'fixture-private-token' not in json.dumps(actual), 'credential leaked'
            counts['fallback' if fallback else 'equal']+=1
    # A changed ordinary answer must fail the very same comparator, including display
    # metadata that does not participate in identity. Preserve the terminal envelope.
    with tempfile.TemporaryDirectory(prefix='aac-m2p4-negative-') as tmp:
        home=Path(tmp); claude(home)
        req=dict(version=1,action='inventory',provider='claude',homeDir=str(home),ccsDir=str(home/'scope'))
        expected=helper.run(req); actual=call(binary,req)
        check(actual,expected,False)
        changed=copy.deepcopy(actual); changed['candidates'][0]['plan']='negative-control-change'
        try: check(changed,expected,False)
        except AssertionError: pass
        else: raise AssertionError('negative control falsely passed')
    with tempfile.TemporaryDirectory(prefix='aac-m2p4-raw-') as tmp:
        home=Path(tmp); codex(home)
        base=dict(version=1,action='inventory',provider='codex',homeDir=str(home),ccsDir=str(home/'scope'))
        expected=helper.run(base)
        assert expected['candidates'], 'raw controls need an ordinary nonempty reference'
        check(call(binary,base),expected,False)
        raw=json.dumps(base)
        duplicate_provider=raw.replace('"provider": "codex"','"provider": "codex", "provider": "codex"')
        duplicate_home=raw.replace('"homeDir": '+json.dumps(str(home)), '"homeDir": '+json.dumps(str(home))+', "homeDir": '+json.dumps(str(home)))
        for args in [duplicate_provider,duplicate_home]:
            try: json.loads(args,object_pairs_hook=helper._unique,parse_constant=helper._invalid)
            except ValueError: pass
            else: raise AssertionError('Python raw reference accepted duplicate keys')
            wire='{"protocol":1,"verb":"failover-inventory","id":"parity","deadlineMs":12000,"args":'+args+'}\n'
            result=subprocess.run([binary],input=wire,text=True,capture_output=True,timeout=18,env={'PATH':os.environ.get('PATH','/usr/bin:/bin')})
            assert terminal(result)=={'fallback':'python'}, 'duplicate raw request must delegate before IO'
        wire=json.dumps(dict(protocol=1,verb='failover-inventory',id='parity',deadlineMs=12000,args=base))+' '*65536+'\n'
        assert len(wire.encode())>helper.MAX_REQUEST
        result=subprocess.run([binary],input=wire,text=True,capture_output=True,timeout=18,env={'PATH':os.environ.get('PATH','/usr/bin:/bin')})
        assert terminal(result)=={'fallback':'python'}, 'raw request cap must delegate before IO'
    print('PASS raw inventory controls: 2 duplicate-key + 1 request-cap fixed fallbacks')
    print('PASS failover-inventory:',counts,'0 mismatches; changed-answer negative control rejected')

if __name__=='__main__': main()
