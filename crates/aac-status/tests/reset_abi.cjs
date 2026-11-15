const fs = require('fs'),
  os = require('os'),
  path = require('path'),
  crypto = require('crypto'),
  assert = require('assert');
// Explicit test artifact and fake app-server only; no provider calls or reset writes.
const moduleFile = process.argv[2];
if (!moduleFile) throw new Error('Pass the absolute path of the test native module.');
const handle = new (require(moduleFile).NativeStatus)();
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-abi-'));
fs.chmodSync(root, 0o700);
const account = 'synthetic-route',
  caller = 'session:' + crypto.randomBytes(32).toString('hex');
const home = path.join(root, 'profile');
const bin = path.join(root, 'bin');
fs.mkdirSync(home);
fs.mkdirSync(bin);
const claims = Buffer.from(
  JSON.stringify({
    email: 'synthetic-principal',
    'https://api.openai.com/auth': { chatgpt_account_id: 'synthetic-account' },
  })
).toString('base64url');
fs.writeFileSync(
  path.join(home, 'auth.json'),
  JSON.stringify({ tokens: { account_id: 'synthetic-account', id_token: `e30.${claims}.fixture` } })
);
fs.writeFileSync(
  path.join(bin, 'codex'),
  `#!/usr/bin/python3
import json,os,sys
home=os.environ['CODEX_HOME']
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'synthetic-principal'}}
 elif method=='account/rateLimits/read': result=json.load(open(os.path.join(home,'quota.json')))
 else:
  open(os.path.join(home,'unexpected-send'),'w').close()
  result={}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
`,
  { mode: 0o700 }
);
process.env.PATH = bin + path.delimiter + process.env.PATH;
const snapshot = {
  windows: { primary: 100 },
  grants: [
    { id: 'exact_a', left: 1 },
    { id: 'exact_b', left: 1 },
  ],
};
async function capturedRead(state) {
  fs.writeFileSync(
    path.join(home, 'quota.json'),
    JSON.stringify({
      accountId: 'synthetic-account',
      rateLimits: { primary: { usedPercent: state.windows.primary } },
      rateLimitResetCredits: {
        availableCount: state.grants.filter((g) => g.left).length,
        credits: state.grants.map((g) => ({
          id: g.id,
          resetType: 'codexRateLimits',
          status: g.left ? 'available' : 'redeemed',
        })),
      },
    })
  );
  return JSON.parse(
    await handle.call(
      'resetCodexSource',
      JSON.stringify({
        dbPath: path.join(root, 'claims.sqlite'),
        routeAccount: account,
        home,
        expectedAccount: 'synthetic-account',
        profile: 'synthetic-profile',
        host: 'fixture-host',
        write: null,
      })
    )
  );
}
async function call(op, args = {}) {
  if (args.snapshot) {
    args = { ...args, snapshot: await capturedRead(args.snapshot) };
  }
  return JSON.parse(
    await handle.call(
      'resetClaims',
      JSON.stringify({
        dbPath: path.join(root, 'claims.sqlite'),
        op,
        account,
        nowMs: Date.now(),
        entropy: crypto.randomBytes(32).toString('hex'),
        ...args,
      })
    )
  );
}
(async () => {
  try {
    const inv = await call('inventory', { snapshot });
    const key = inv.grants[1].grantKey;
    await call('prepareGate', { key });
    const prepared = await call('prepare', { key, caller, snapshot });
    assert.equal(prepared.account.label, 'synthetic-principal');
    await call('claimGate', { key });
    const write = await call('begin', { key, caller, token: prepared.confirmationToken, snapshot });
    assert.equal(write.body.creditId, 'exact_b');
    assert.equal(write.routeAccount, account);
    await assert.rejects(call('prepareGate', { key }));
    await assert.rejects(
      call('begin', { key, caller, token: prepared.confirmationToken, snapshot })
    );
    const first = await call('finish', { key: write.attemptKey, outcome: 'reset', snapshot: null });
    assert.equal(first.outcome, 'claimed-unconfirmed');
    assert.equal(first.providerSuccess, true);
    const after = structuredClone(snapshot);
    after.observedAtMs = Date.now() + 1;
    await new Promise((r) => setTimeout(r, 5));
    after.grants[1].left = 0;
    after.windows.primary = 0;
    const checked = await call('check', { key: write.attemptKey, snapshot: after });
    assert.equal(checked.outcome, 'claimed');
    assert.equal(checked.providerSuccess, true);
    assert(!fs.existsSync(path.join(home, 'unexpected-send')));
    const history = JSON.stringify(await call('history'));
    assert(!history.includes('exact_b'));
    assert(!history.includes('synthetic-principal'));
    console.log(
      'PASS: actual native ABI inventory/prepare/exact begin/one-shot/ambiguous result/read-only check/audit privacy'
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
})().catch((e) => {
  console.error('FAIL: offline native ABI');
  process.exitCode = 1;
});
