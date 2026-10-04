import test from 'node:test';
import assert from 'node:assert/strict';
import { createAntigravityConfirmation } from '../public/antigravity-confirmation.mjs';

const NOW = Date.parse('2026-10-01T22:00:00.000Z');
const PROFILE = 'antigravity-primary';
const EMAIL = 'account@example.invalid';
const TOKEN = 'privateToken_1234567890abcdefXYZ';
const NEXT_TOKEN = 'replacementToken_1234567890abcdef';
const HOST = 'ubuntu';

function offer() {
  return {
    status: 'confirmation-required',
    profileId: PROFILE,
    hostId: HOST,
    email: EMAIL,
    confirmation: {
      token: TOKEN,
      expiresAt: new Date(NOW + 60_000).toISOString(),
      profileId: PROFILE,
      hostId: HOST,
      email: EMAIL,
      warning: 'Untrusted server warning must not become approval text.',
      processes: [
        { pid: 101, role: 'cli', label: 'Untrusted CLI name' },
        { pid: 102, role: 'desktop', label: 'Untrusted desktop name' },
        { pid: 103, role: 'language-server', label: 'Untrusted server name' },
      ],
    },
  };
}

function active(extra = {}) {
  return { status: 'active', profileId: PROFILE, hostId: HOST, email: EMAIL, ...extra };
}

function httpError(status, payload) {
  return Object.assign(new Error('Synthetic HTTP failure'), { status, payload });
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness({ activate, confirm, recover, success, prompt, now = NOW } = {}) {
  const calls = { activate: [], confirm: [], recover: [], prompt: [], close: [], busy: [], success: [], error: [] };
  let clock = now;
  const controller = createAntigravityConfirmation({
    activate: async (...args) => {
      calls.activate.push(args);
      if (activate) return activate(...args);
      throw httpError(409, offer());
    },
    confirm: async (...args) => {
      calls.confirm.push(args);
      return confirm ? confirm(...args) : active();
    },
    ...(recover === undefined ? {} : {
      recover: async (...args) => {
        calls.recover.push(args);
        return recover(...args);
      },
    }),
    prompt: value => {
      calls.prompt.push(structuredClone(value));
      if (prompt) prompt(value);
    },
    close: (...args) => calls.close.push(args),
    busy: value => calls.busy.push(value),
    success: async (...args) => {
      calls.success.push(args);
      if (success) return success(...args);
    },
    error: (...args) => calls.error.push(args),
    now: () => clock,
  });
  return { controller, calls, setNow: value => { clock = value; } };
}

function assertNoToken(prompts, tokens = [TOKEN, NEXT_TOKEN]) {
  const json = JSON.stringify(prompts);
  for (const token of tokens) assert.equal(json.includes(token), false, 'approval token stays private');
  for (const view of prompts) {
    assert.equal(Object.hasOwn(view, 'token'), false);
    assert.equal(Object.hasOwn(view, 'confirmationToken'), false);
  }
}

async function rejectedOffer(mutate, status = 409) {
  const payload = offer();
  mutate(payload);
  const h = harness({ activate: async () => { throw httpError(status, payload); } });
  await h.controller.begin(PROFILE);
  assert.equal(h.controller.hasPending(), false);
  assert.equal(h.calls.prompt.length, 0);
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.calls.success.length, 0);
  assert.equal(h.calls.error.length, 1);
  assert.deepEqual(h.calls.activate, [[PROFILE, { hostId: HOST }]]);
  assert.deepEqual(h.calls.busy, [true, false]);
}

test('manual activation accepts active and already-active without requesting approval', async t => {
  for (const status of ['active', 'already-active']) {
    await t.test(status, async () => {
      const h = harness({ activate: async () => active({ status }) });
      await h.controller.begin(PROFILE);
      assert.deepEqual(h.calls.activate, [[PROFILE, { hostId: HOST }]]);
      assert.equal(h.calls.confirm.length, 0);
      assert.equal(h.calls.prompt.length, 0);
      assert.equal(h.calls.success.length, 1);
      assert.equal(h.calls.error.length, 0);
      assert.equal(h.controller.hasPending(), false);
      assert.deepEqual(h.calls.busy, [true, false]);
    });
  }
});

test('success may omit email when the typed response does not provide one', async () => {
  const payload = active();
  delete payload.email;
  const h = harness({ activate: async () => payload });
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.success.length, 1);
  assert.equal(h.calls.error.length, 0);
});

test('valid approval uses the separate Ubuntu confirm callback and canonical process copy', async () => {
  const h = harness();
  await h.controller.begin(PROFILE);
  assert.equal(h.controller.hasPending(), true);
  assert.equal(h.calls.prompt.length, 1);
  const view = h.calls.prompt[0];
  assert.equal(view.product, 'Antigravity');
  assert.equal(view.targetProfile, EMAIL);
  assert.equal(view.profileId, PROFILE);
  assert.equal(view.expiresAt, offer().confirmation.expiresAt);
  assert.equal(view.canConfirm, true);
  assert.equal(view.inProgress, false);
  assert.deepEqual(view.processes, [
    { pid: 101, role: 'cli', label: 'Antigravity CLI' },
    { pid: 102, role: 'desktop', label: 'Antigravity Desktop' },
    { pid: 103, role: 'language-server', label: 'Antigravity language server' },
  ]);
  assert.equal(JSON.stringify(view).includes('Untrusted'), false);
  assertNoToken(h.calls.prompt);
  await h.controller.confirm();
  assert.deepEqual(h.calls.confirm, [[PROFILE, { hostId: HOST, confirmationToken: TOKEN }]]);
  assert.equal(h.calls.activate.length, 1);
  assert.equal(h.calls.success.length, 1);
  assert.equal(h.controller.hasPending(), false);
  assertNoToken(h.calls.prompt);
});

test('base64url token length boundaries remain valid', async t => {
  for (const length of [16, 256]) {
    await t.test(String(length), async () => {
      const payload = offer();
      payload.confirmation.token = 'A'.repeat(length);
      const h = harness({ activate: async () => { throw httpError(409, payload); } });
      await h.controller.begin(PROFILE);
      assert.equal(h.controller.hasPending(), true);
      await h.controller.confirm();
      assert.deepEqual(h.calls.confirm, [[PROFILE, { hostId: HOST, confirmationToken: 'A'.repeat(length) }]]);
      assertNoToken(h.calls.prompt, ['A'.repeat(length)]);
    });
  }
});

test('reject malformed token, expiry, status and cross-context approval offers', async t => {
  const cases = [
    ['short token', p => { p.confirmation.token = 'A'.repeat(15); }],
    ['long token', p => { p.confirmation.token = 'A'.repeat(257); }],
    ['non-base64url token', p => { p.confirmation.token = 'A'.repeat(16) + '/'; }],
    ['padded token', p => { p.confirmation.token = 'A'.repeat(16) + '='; }],
    ['whitespace token', p => { p.confirmation.token = 'A'.repeat(16) + ' '; }],
    ['numeric token', p => { p.confirmation.token = 1234567890123456; }],
    ['missing token', p => { delete p.confirmation.token; }],
    ['missing confirmation', p => { delete p.confirmation; }],
    ['wrong status', p => { p.status = 'active'; }],
    ['missing status', p => { delete p.status; }],
    ['outer profile mismatch', p => { p.profileId = 'other-profile'; }],
    ['nested profile mismatch', p => { p.confirmation.profileId = 'other-profile'; }],
    ['outer host mismatch', p => { p.hostId = 'mac'; }],
    ['nested host mismatch', p => { p.confirmation.hostId = 'windows'; }],
    ['outer host missing', p => { delete p.hostId; }],
    ['nested host missing', p => { delete p.confirmation.hostId; }],
    ['nested email mismatch', p => { p.confirmation.email = 'other@example.invalid'; }],
    ['outer email invalid', p => { p.email = 'invalid'; }],
    ['nested email invalid', p => { p.confirmation.email = 'invalid'; }],
    ['email missing', p => { delete p.email; delete p.confirmation.email; }],
    ['email with whitespace', p => { p.email = p.confirmation.email = 'person @example.invalid'; }],
    ['email with control character', p => { p.email = p.confirmation.email = 'person\n@example.invalid'; }],
    ['email too long', p => { p.email = p.confirmation.email = 'a'.repeat(250) + '@example.invalid'; }],
    ['invalid expiry', p => { p.confirmation.expiresAt = 'not a date'; }],
    ['non-ISO parseable expiry', p => { p.confirmation.expiresAt = 'October 2, 2026 22:00:00 UTC'; }],
    ['year beyond permitted range', p => { p.confirmation.expiresAt = '2201-10-01T22:00:00.000Z'; }],
    ['expiry too long', p => { p.confirmation.expiresAt = '2026-10-02T22:00:00.000Z' + ' '.repeat(20); }],
    ['non-string expiry', p => { p.confirmation.expiresAt = NOW + 60_000; }],
    ['missing expiry', p => { delete p.confirmation.expiresAt; }],
    ['expired first offer', p => { p.confirmation.expiresAt = new Date(NOW - 1).toISOString(); }],
    ['expiry exactly now', p => { p.confirmation.expiresAt = new Date(NOW).toISOString(); }],
  ];
  for (const [name, mutate] of cases) await t.test(name, () => rejectedOffer(mutate));
});

test('every listed process must be valid, within PID bounds and the maximum count', async t => {
  const cases = [
    ['missing list', p => { delete p.confirmation.processes; }],
    ['non-array list', p => { p.confirmation.processes = {}; }],
    ['null entry', p => { p.confirmation.processes[1] = null; }],
    ['zero PID', p => { p.confirmation.processes[1].pid = 0; }],
    ['negative PID', p => { p.confirmation.processes[1].pid = -1; }],
    ['fractional PID', p => { p.confirmation.processes[1].pid = 1.5; }],
    ['string PID', p => { p.confirmation.processes[1].pid = '102'; }],
    ['NaN PID', p => { p.confirmation.processes[1].pid = NaN; }],
    ['out-of-range PID', p => { p.confirmation.processes[1].pid = 2147483648; }],
    ['unknown role', p => { p.confirmation.processes[1].role = 'other'; }],
    ['missing role', p => { delete p.confirmation.processes[1].role; }],
    ['33 processes', p => { p.confirmation.processes = Array.from({ length: 33 }, (_, i) => ({ pid: i + 1, role: 'cli' })); }],
  ];
  for (const [name, mutate] of cases) await t.test(name, () => rejectedOffer(mutate));
});

test('PID bounds and exactly 32 valid processes are accepted without trusting server labels', async () => {
  const payload = offer();
  payload.confirmation.processes = Array.from({ length: 32 }, (_, i) => ({
    pid: i === 31 ? 2147483647 : i + 1,
    role: 'cli',
    label: { attackerControlled: TOKEN },
  }));
  const h = harness({ activate: async () => { throw httpError(409, payload); } });
  await h.controller.begin(PROFILE);
  assert.equal(h.controller.hasPending(), true);
  assert.equal(h.calls.prompt[0].processes.length, 32);
  assert.equal(h.calls.prompt[0].processes[0].pid, 1);
  assert.equal(h.calls.prompt[0].processes[31].pid, 2147483647);
  assert.equal(h.calls.prompt[0].processes.every(p => p.label === 'Antigravity CLI'), true);
  assertNoToken(h.calls.prompt);
});

test('an explicitly empty valid process inventory is retained', async () => {
  const payload = offer();
  payload.confirmation.processes = [];
  const h = harness({ activate: async () => { throw httpError(409, payload); } });
  await h.controller.begin(PROFILE);
  assert.equal(h.controller.hasPending(), true);
  assert.deepEqual(h.calls.prompt[0].processes, []);
});

test('invalid profile IDs never reach either remote callback', async t => {
  for (const id of ['', '_leading', '-leading', 'has space', 'path/profile', 'a'.repeat(65), null, 123, {}]) {
    await t.test(JSON.stringify(id), async () => {
      const h = harness();
      await h.controller.begin(id);
      assert.equal(h.calls.activate.length, 0);
      assert.equal(h.calls.confirm.length, 0);
      assert.equal(h.calls.prompt.length, 0);
      assert.equal(h.calls.success.length, 0);
      assert.equal(h.controller.hasPending(), false);
    });
  }
});

test('64-character profile IDs are passed intact and bound to the matching response', async () => {
  const id = 'a'.repeat(64);
  const h = harness({ activate: async target => active({ profileId: target }) });
  await h.controller.begin(id);
  assert.deepEqual(h.calls.activate, [[id, { hostId: HOST }]]);
  assert.equal(h.calls.success.length, 1);
});

test('HTTP 401 and string HTTP status never open an approval even with a valid offer', async t => {
  await t.test('401', () => rejectedOffer(() => {}, 401));
  await t.test('string 409', () => rejectedOffer(() => {}, '409'));
});

test('pending review blocks further manual offers and a completed review cannot be submitted twice', async () => {
  const h = harness();
  await h.controller.begin(PROFILE);
  await h.controller.begin('other-profile');
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.activate.length, 1);
  assert.equal(h.calls.prompt.length, 1);
  await h.controller.confirm();
  await h.controller.confirm();
  assert.equal(h.calls.confirm.length, 1);
  assert.equal(h.calls.success.length, 1);
});

test('unresolved activation guards begin, cancel and confirm without changing the bound target', async () => {
  const request = deferred();
  const h = harness({ activate: () => request.promise });
  const first = h.controller.begin(PROFILE);
  await h.controller.begin('other-profile');
  await h.controller.begin(PROFILE);
  h.controller.cancel();
  await h.controller.confirm();
  h.controller.expire();
  assert.deepEqual(h.calls.activate, [[PROFILE, { hostId: HOST }]]);
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.calls.close.length, 0);
  request.reject(httpError(409, offer()));
  await first;
  assert.equal(h.controller.hasPending(), true);
  assert.equal(h.calls.prompt.at(-1).profileId, PROFILE);
});

test('unresolved approval guards double-click, cancel, begin and expiry until settlement', async () => {
  const request = deferred();
  const h = harness({ confirm: () => request.promise });
  await h.controller.begin(PROFILE);
  const approval = h.controller.confirm();
  await h.controller.confirm();
  h.controller.cancel();
  await h.controller.begin('other-profile');
  h.setNow(NOW + 120_000);
  h.controller.expire();
  assert.equal(h.calls.confirm.length, 1);
  assert.equal(h.calls.activate.length, 1);
  assert.equal(h.calls.close.length, 0);
  assert.equal(h.controller.hasPending(), true);
  request.resolve(active());
  await approval;
  assert.equal(h.calls.success.length, 1);
  assert.equal(h.controller.hasPending(), false);
  assert.deepEqual(h.calls.busy, [true, false, true, false]);
  assertNoToken(h.calls.prompt);
});

test('expiry is rechecked before approval submission and requires a new manual offer', async () => {
  const h = harness();
  await h.controller.begin(PROFILE);
  h.setNow(NOW + 60_000);
  await h.controller.confirm();
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.calls.activate.length, 1);
  assert.equal(h.controller.hasPending(), true);
  assert.equal(h.calls.prompt.at(-1).canConfirm, false);
  h.controller.expire();
  assert.equal(h.calls.activate.length, 1);
  h.controller.cancel();
  assert.equal(h.controller.hasPending(), false);
  h.setNow(NOW);
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.activate.length, 2);
  assert.equal(h.controller.hasPending(), true);
});

test('cancel closes an idle review without approval or success and allows a fresh begin', async () => {
  const h = harness();
  await h.controller.begin(PROFILE);
  h.controller.cancel();
  assert.equal(h.controller.hasPending(), false);
  assert.equal(h.calls.close.length, 1);
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.calls.success.length, 0);
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.activate.length, 2);
});

test('failed approvals, including another valid 409, invalidate the token without retrying', async t => {
  for (const status of [401, 409, 500]) {
    await t.test(String(status), async () => {
      const replacement = offer();
      replacement.confirmation.token = NEXT_TOKEN;
      const h = harness({ confirm: async () => { throw httpError(status, replacement); } });
      await h.controller.begin(PROFILE);
      await h.controller.confirm();
      assert.equal(h.controller.hasPending(), true);
      assert.equal(h.calls.prompt.at(-1).canConfirm, false);
      assert.equal(h.calls.success.length, 0);
      await h.controller.confirm();
      h.controller.expire();
      await h.controller.begin(PROFILE);
      assert.equal(h.calls.confirm.length, 1);
      assert.equal(h.calls.activate.length, 1);
      assert.deepEqual(h.calls.confirm[0], [PROFILE, { hostId: HOST, confirmationToken: TOKEN }]);
      assertNoToken(h.calls.prompt);
      h.controller.cancel();
      await h.controller.begin(PROFILE);
      assert.equal(h.calls.activate.length, 2);
      assert.equal(h.controller.hasPending(), true);
    });
  }
});

test('invalid activation success is rejected instead of completing or offering approval', async t => {
  const cases = [
    ['wrong status', p => { p.status = 'confirmation-required'; }],
    ['missing status', p => { delete p.status; }],
    ['wrong profile', p => { p.profileId = 'other-profile'; }],
    ['wrong host', p => { p.hostId = 'mac'; }],
    ['missing host', p => { delete p.hostId; }],
    ['invalid supplied email', p => { p.email = 'invalid'; }],
  ];
  for (const [name, mutate] of cases) {
    await t.test(name, async () => {
      const payload = active();
      mutate(payload);
      const h = harness({ activate: async () => payload });
      await h.controller.begin(PROFILE);
      assert.equal(h.calls.success.length, 0);
      assert.equal(h.calls.prompt.length, 0);
      assert.equal(h.calls.confirm.length, 0);
      assert.equal(h.calls.error.length, 1);
      assert.equal(h.controller.hasPending(), false);
    });
  }
});

test('mismatched approval success invalidates the review and cannot resend its token', async t => {
  for (const extra of [{ profileId: 'other-profile' }, { hostId: 'windows' }, { status: 'unknown' }, { email: 'invalid' }, { email: 'other@example.invalid' }]) {
    await t.test(JSON.stringify(extra), async () => {
      const h = harness({ confirm: async () => active(extra) });
      await h.controller.begin(PROFILE);
      await h.controller.confirm();
      assert.equal(h.calls.success.length, 0);
      assert.equal(h.controller.hasPending(), true);
      assert.equal(h.calls.prompt.at(-1).canConfirm, false);
      await h.controller.confirm();
      assert.equal(h.calls.confirm.length, 1);
      assertNoToken(h.calls.prompt);
    });
  }
});

test('asynchronous success delivery keeps manual actions locked until it settles', async () => {
  const delivery = deferred();
  const enteredSuccess = deferred();
  const h = harness({ activate: async () => active(), success: () => {
    enteredSuccess.resolve();
    return delivery.promise;
  } });
  const first = h.controller.begin(PROFILE);
  await enteredSuccess.promise;
  assert.equal(h.calls.success.length, 1);
  await h.controller.begin('other-profile');
  h.controller.cancel();
  await h.controller.confirm();
  assert.equal(h.calls.activate.length, 1);
  assert.equal(h.calls.confirm.length, 0);
  assert.deepEqual(h.calls.busy, [true]);
  delivery.resolve();
  await first;
  assert.deepEqual(h.calls.busy, [true, false]);
});

test('successful callback receives only safe response fields, without tokens or unknown properties', async () => {
  const payload = active({ confirmationToken: TOKEN, token: NEXT_TOKEN, unknown: { secret: TOKEN } });
  const h = harness({ activate: async () => payload });
  await h.controller.begin(PROFILE);
  assert.deepEqual(h.calls.success, [[active()]]);
  assert.equal(JSON.stringify(h.calls.success).includes(TOKEN), false);
  assert.equal(JSON.stringify(h.calls.success).includes(NEXT_TOKEN), false);
});

test('prompt consumers cannot mutate the private approval target or expiry', async () => {
  const h = harness({ prompt: view => {
    view.profileId = 'other-profile';
    view.targetProfile = 'other@example.invalid';
    view.expiresAt = new Date(NOW - 1).toISOString();
    if (view.processes[0]) view.processes[0].pid = -1;
  } });
  await h.controller.begin(PROFILE);
  await h.controller.confirm();
  assert.deepEqual(h.calls.confirm, [[PROFILE, { hostId: HOST, confirmationToken: TOKEN }]]);
  assert.equal(h.calls.success.length, 1);
  assertNoToken(h.calls.prompt);
});

test('approval tokens embedded in display identities never reach a prompt', async t => {
  await t.test('email identity', async () => {
    await rejectedOffer(payload => {
      payload.email = payload.confirmation.email = TOKEN + '@example.invalid';
    });
  });
  await t.test('profile identity', async () => {
    const payload = offer();
    payload.profileId = payload.confirmation.profileId = TOKEN;
    const h = harness({ activate: async () => { throw httpError(409, payload); } });
    await h.controller.begin(TOKEN);
    assert.equal(h.controller.hasPending(), false);
    assert.equal(h.calls.prompt.length, 0);
    assert.equal(h.calls.confirm.length, 0);
    assert.equal(h.calls.success.length, 0);
  });
});

test('a private approval token is redacted from failure messages', async () => {
  const failure = Object.assign(new Error('Request failed with token ' + TOKEN), { status: 500 });
  const h = harness({ confirm: async () => { throw failure; } });
  await h.controller.begin(PROFILE);
  await h.controller.confirm();
  assert.equal(h.calls.prompt.at(-1).canConfirm, false);
  assertNoToken(h.calls.prompt);
  assert.equal(JSON.stringify(h.calls.error).includes(TOKEN), false);
});

test('typed failures replace a generic HTTP error with fixed meaningful copy without an offer or retry', async t => {
  for (const status of ['busy', 'deferred', 'unsupported-runtime-probe', 'stale-confirmation', 'failed-rolled-back', 'recovery-required']) {
    await t.test(status, async () => {
      const payload = {
        status,
        profileId: PROFILE,
        hostId: HOST,
        reason: 'Untrusted raw reason ' + TOKEN,
        confirmationToken: TOKEN,
      };
      const failure = Object.assign(new Error('Request failed (409).'), { status: 409, payload });
      const h = harness({ activate: async () => { throw failure; } });
      await h.controller.begin(PROFILE);
      assert.equal(h.controller.hasPending(), false);
      assert.equal(h.calls.prompt.length, 0);
      assert.equal(h.calls.confirm.length, 0);
      assert.equal(h.calls.success.length, 0);
      assert.equal(h.calls.activate.length, 1);
      assert.equal(h.calls.error.length, 1);
      const message = h.calls.error[0][0];
      assert.equal(typeof message, 'string');
      assert.notEqual(message, 'Request failed (409).');
      assert.ok(message.length > 0);
      assert.equal(message.includes('Untrusted raw reason'), false);
      assert.equal(message.includes(TOKEN), false);
      h.controller.expire();
      await h.controller.confirm();
      assert.equal(h.calls.activate.length, 1);
      assert.equal(h.calls.confirm.length, 0);
    });
  }
});

test('unknown typed status preserves a generic error instead of displaying a server reason', async () => {
  const failure = Object.assign(new Error('Request failed (409).'), {
    status: 409,
    payload: { status: 'unknown-status', reason: 'Untrusted raw reason ' + TOKEN },
  });
  const h = harness({ activate: async () => { throw failure; } });
  await h.controller.begin(PROFILE);
  assert.deepEqual(h.calls.error, [['Request failed (409).']]);
  assert.equal(h.calls.prompt.length, 0);
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.controller.hasPending(), false);
});

test('an explicit safe activation error is preserved even with a mapped status', async () => {
  const message = 'Please wait for the verified Ubuntu transaction.';
  const failure = Object.assign(new Error(message), {
    status: 409,
    payload: { status: 'busy', reason: 'Untrusted raw reason ' + TOKEN },
  });
  const h = harness({ activate: async () => { throw failure; } });
  await h.controller.begin(PROFILE);
  assert.deepEqual(h.calls.error, [[message]]);
  assert.equal(h.calls.prompt.length, 0);
  assert.equal(h.calls.confirm.length, 0);
});

test('typed activation failure copy still filters private tokens in an explicit error', async () => {
  const payload = offer();
  payload.status = 'busy';
  payload.reason = 'Untrusted raw reason ' + NEXT_TOKEN;
  const failure = Object.assign(new Error('Approval token ' + TOKEN), { status: 409, payload });
  const h = harness({ activate: async () => { throw failure; } });
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.error.length, 1);
  assert.equal(JSON.stringify(h.calls.error).includes(TOKEN), false);
  assert.equal(JSON.stringify(h.calls.error).includes(NEXT_TOKEN), false);
  assert.equal(h.calls.prompt.length, 0);
  assert.equal(h.calls.confirm.length, 0);
  assert.equal(h.controller.hasPending(), false);
});

test('a stuck switch offers a guarded recovery instead of an error toast', async () => {
  const h = harness({
    activate: async () => { throw httpError(500, { status: 'recovery-required', profileId: PROFILE, hostId: HOST }); },
    recover: async () => ({ status: 'completed', hostId: HOST, profileId: PROFILE, email: EMAIL }),
  });
  await h.controller.begin(PROFILE);
  assert.equal(h.calls.error.length, 0);
  assert.equal(h.controller.hasPending(), true);
  const dialog = h.calls.prompt.at(-1);
  assert.equal(dialog.profileId, PROFILE);
  assert.equal(dialog.processes.length, 0);
  assert.match(dialog.warning, /stuck/);
  assert.match(dialog.warning, /Saved profiles are not changed/);
  assert.equal(dialog.canConfirm, true);
  await h.controller.confirm();
  assert.equal(h.calls.recover.length, 1);
  assert.deepEqual(h.calls.success, [[{ status: 'completed', hostId: HOST, profileId: PROFILE, email: EMAIL }]]);
  assert.equal(h.calls.close.length, 1);
  assert.equal(h.controller.hasPending(), false);
});

test('a recovery that stays stuck reports in the dialog and needs a new Activate', async () => {
  const stuck = () => Object.assign(new Error('Request failed (500).'), {
    status: 500, payload: { status: 'recovery-required', profileId: PROFILE, hostId: HOST },
  });
  const h = harness({
    activate: async () => { throw stuck(); },
    recover: async () => { throw stuck(); },
  });
  await h.controller.begin(PROFILE);
  await h.controller.confirm();
  assert.equal(h.calls.recover.length, 1);
  assert.equal(h.calls.success.length, 0);
  const dialog = h.calls.prompt.at(-1);
  assert.match(dialog.error, /needs recovery/);
  assert.equal(dialog.canConfirm, false);
  await h.controller.confirm();
  assert.equal(h.calls.recover.length, 1);
});

test('without a recovery action a stuck switch keeps the old error toast', async () => {
  const stuck = Object.assign(new Error('Request failed (500).'), {
    status: 500, payload: { status: 'recovery-required', profileId: PROFILE, hostId: HOST },
  });
  const h = harness({
    activate: async () => { throw stuck; },
  });
  await h.controller.begin(PROFILE);
  assert.equal(h.controller.hasPending(), false);
  assert.equal(h.calls.prompt.length, 0);
  assert.equal(h.calls.error.length, 1);
  assert.match(h.calls.error[0][0], /needs recovery/);
});
