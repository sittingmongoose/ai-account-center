import test from 'node:test';
import assert from 'node:assert/strict';
import { createActivationConfirmation } from '../public/activation-confirmation.mjs';
const grant = { token: 'a'.repeat(43), targetProfile: 'party', expiresAt: '2026-10-01T12:05:00Z', warning: 'Stopping these programs interrupts active Codex work.', processes: [{ label: 'Codex desktop', pid: 123, role: 'desktop' }] };
const failure = confirmation => Object.assign(new Error('Running programs need review.'), { status: 409, payload: { confirmation } });
function harness(activate) {
  let time = Date.parse('2026-10-01T12:00:00Z');
  const prompts = [], errors = [], successes = [], busyStates = [];
  let closed = 0;
  const controller = createActivationConfirmation({ activate, prompt: view => prompts.push(view), close: () => closed++, busy: state => busyStates.push(state), success: result => successes.push(result), error: error => errors.push(error), now: () => time });
  return { controller, prompts, errors, successes, busyStates, closed: () => closed, expire: () => { time += 600_000; } };
}
test('initial busy reply opens a reviewable dialog and keeps token out of the view model', async () => {
  const h = harness(async () => { throw failure(grant); });
  await h.controller.begin('party');
  assert.equal(h.prompts.at(-1).targetProfile, 'party');
  assert.equal(h.prompts.at(-1).processes[0].pid, 123);
  assert.equal(h.prompts.at(-1).canConfirm, true);
  assert.equal('token' in h.prompts.at(-1), false);
  assert.deepEqual(h.busyStates, [true, false]);
});
test('cancel does not send an approval mutation', async () => {
  const calls = [];
  const h = harness(async (target, body) => { calls.push({ target, body }); throw failure(grant); });
  await h.controller.begin('party'); assert.equal(h.controller.cancel(), true);
  await h.controller.confirm();
  assert.deepEqual(calls, [{ target: 'party', body: {} }]);
  assert.equal(h.controller.hasPending(), false);
});
test('yes sends only the reviewed token to the same target and reports success', async () => {
  const calls = [];
  const h = harness(async (target, body) => { calls.push({ target, body }); if (calls.length === 1) throw failure(grant); return { success: true, name: 'party' }; });
  await h.controller.begin('party'); await h.controller.confirm();
  assert.deepEqual(calls[1], { target: 'party', body: { confirmationToken: grant.token } });
  assert.equal(h.successes.length, 1); assert.equal(h.controller.hasPending(), false);
});
test('expired review cannot stop or restart programs', async () => {
  let calls = 0;
  const h = harness(async () => { calls++; throw failure(grant); });
  await h.controller.begin('party'); h.expire(); h.controller.expire(); await h.controller.confirm();
  assert.equal(calls, 1); assert.equal(h.prompts.at(-1).canConfirm, false);
  assert.match(h.prompts.at(-1).error, /expired/);
});
test('a changed process/account rejection requires a new review and never retries automatically', async () => {
  let calls = 0;
  const h = harness(async () => { if (++calls === 1) throw failure(grant); throw Object.assign(new Error('Programs changed. Activate again.'), { status: 409, payload: { code: 'confirmation_stale' } }); });
  await h.controller.begin('party'); await h.controller.confirm(); await h.controller.confirm();
  assert.equal(calls, 2); assert.equal(h.prompts.at(-1).canConfirm, false);
  assert.equal(h.prompts.at(-1).error, 'Programs changed. Activate again.');
});
test('wrong target grant never opens an approval dialog', async () => {
  const h = harness(async () => { throw failure({ ...grant, targetProfile: 'gmail' }); });
  await h.controller.begin('party'); assert.equal(h.prompts.length, 0); assert.equal(h.errors.length, 1);
});
test('approval and cancel are guarded while the switch is running', async () => {
  let resolve;
  let calls = 0;
  const h = harness(async () => { if (++calls === 1) throw failure(grant); return new Promise(r => { resolve = r; }); });
  await h.controller.begin('party'); const approval = h.controller.confirm();
  assert.equal(h.controller.cancel(), false); await h.controller.confirm(); assert.equal(calls, 2);
  assert.equal(h.prompts.at(-1).inProgress, true);
  resolve({ success: true, name: 'party' }); await approval;
});
test('an idle activation succeeds directly without a confirmation dialog', async () => {
  const h = harness(async () => ({ success: true, name: 'party' }));
  await h.controller.begin('party'); assert.equal(h.prompts.length, 0); assert.equal(h.successes.length, 1);
});
