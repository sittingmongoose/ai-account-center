const fs = require('fs'),
  os = require('os'),
  path = require('path'),
  crypto = require('crypto'),
  assert = require('assert');
// Explicit test artifact only. This scenario never calls resetCodexSource or a provider.
const moduleFile = process.argv[2];
if (!moduleFile) throw new Error('Pass the absolute path of the test native module.');
const handle = new (require(moduleFile).NativeStatus)();
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-abi-'));
fs.chmodSync(root, 0o700);
const account = 'synthetic-route',
  caller = 'session:' + crypto.randomBytes(32).toString('hex');
const snapshot = {
  identity: {
    provider: 'codex',
    account: 'synthetic-account',
    organization: '',
    profile: 'synthetic-profile',
    host: 'fixture-host',
  },
  observedAtMs: Date.now(),
  grants: [
    {
      id: 'exact_a',
      kind: 'banked',
      clears: ['primary'],
      left: 1,
      expiresAtMs: null,
      usable: true,
    },
    {
      id: 'exact_b',
      kind: 'banked',
      clears: ['primary'],
      left: 1,
      expiresAtMs: null,
      usable: true,
    },
  ],
  windows: { primary: 100 },
  executorReady: true,
  sourceBinding: 'synthetic-binding',
  accountLabel: 'synthetic-principal',
};
async function call(op, args = {}) {
  if (args.snapshot) {
    args = { ...args, snapshot: { ...args.snapshot, readTicket: await call('readTicket') } };
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
