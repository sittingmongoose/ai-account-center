import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createClaudeOpen, openProgress, openProgressText, operationOf, pollDelay, validOperation,
  FAST_POLL_MS, SLOW_POLL_MS, FAST_FOR_MS, POLL_LIMIT_MS, HOLD_OPENED_MS, OPEN_SENTENCES,
} from '../public/claude-open.mjs';
import { dashboardViewModel, detailsViewModel } from '../public/view-model.mjs';
import { accountsViewModel } from '../public/accounts-view.mjs';

// A fake clock and timer queue: run() fires the earliest timer and advances the clock to it.
function fakeTimers(start = 1_000_000) {
  let now = start, seq = 0;
  const timers = new Map();
  return {
    clock: () => now,
    schedule: (fn, ms) => { const id = ++seq; timers.set(id, { at: now + ms, fn }); return id; },
    cancel: id => { timers.delete(id); },
    pending: () => timers.size,
    async run() {
      const [id, timer] = [...timers.entries()].sort((a, b) => a[1].at - b[1].at)[0] || [];
      if (!timer) return false;
      timers.delete(id); now = timer.at; await timer.fn();
      // let the async tick finish its awaits
      for (let i = 0; i < 5; i++) await Promise.resolve();
      return true;
    },
    advance(ms) { now += ms; },
  };
}
const op = (state, extra = {}) => ({ id: 'op-1', platform: 'mac', state, confirmedCount: null, totalCount: null, message: null, ...extra });
const listing = (operation, id = 'party') => ({ profiles: [{ id: 'gmail', email: 'g@example.test', openOperation: null }, { id, email: 'p@example.test', openOperation: operation }] });

function harness(answers, post = async () => ({ status: 202, body: { id: 'party', platform: 'mac', state: 'checking', operationId: 'op-1' } })) {
  const timers = fakeTimers();
  const calls = { post: 0, list: 0, changed: 0, finished: [] };
  const queue = [...answers];
  const controller = createClaudeOpen({
    post: async (...args) => { calls.post++; return post(...args); },
    list: async () => {
      calls.list++;
      const next = queue.length > 1 ? queue.shift() : queue[0];
      if (next instanceof Error) throw next;
      return next;
    },
    schedule: timers.schedule, cancel: timers.cancel, clock: timers.clock,
    changed: () => { calls.changed++; },
    finished: (id, view) => { calls.finished.push({ id, ...view }); },
  });
  return { controller, timers, calls };
}

test('the progress line follows the operation and never shows ids or paths', () => {
  assert.equal(openProgressText(op('checking')), 'Checking history on Mac');
  assert.equal(openProgressText(op('copying')), 'Copying history');
  assert.equal(openProgressText(op('copying', { confirmedCount: 3, totalCount: 18 })), 'Copying history 3 of 18');
  // a count past the total is never shown
  assert.equal(openProgressText(op('copying', { confirmedCount: 20, totalCount: 18 })), 'Copying history 18 of 18');
  assert.equal(openProgressText(op('opening', { platform: 'windows' })), 'Opening on Windows');
  assert.equal(openProgressText(op('opened')), 'Opened on Mac');
  assert.equal(openProgressText(op('failed', { message: 'Claude desktop request timed out.' })), 'Claude desktop request timed out.');
  assert.equal(openProgressText(op('failed')), OPEN_SENTENCES.failed);
  assert.equal(openProgressText(op('blocked_uncertain')), OPEN_SENTENCES.blocked_uncertain);
});

test('malformed operations are ignored', () => {
  assert.equal(validOperation(null), null);
  assert.equal(validOperation(op('done')), null);
  assert.equal(validOperation(op('copying', { platform: 'linux' })), null);
  assert.equal(validOperation(op('copying', { id: '' })), null);
  assert.deepEqual(validOperation(op('copying', { confirmedCount: -1, totalCount: 2.5 })), op('copying'));
  assert.equal(operationOf(listing(op('opening')), 'party').state, 'opening');
  assert.equal(operationOf(listing(op('opening')), 'me'), null);
  assert.equal(operationOf({ profiles: 'nope' }, 'party'), null);
});

test('polls every second for two minutes, then every five seconds, and stops at the limit', () => {
  assert.equal(pollDelay(0), FAST_POLL_MS);
  assert.equal(pollDelay(FAST_FOR_MS - 1), FAST_POLL_MS);
  assert.equal(pollDelay(FAST_FOR_MS), SLOW_POLL_MS);
  assert.equal(pollDelay(POLL_LIMIT_MS - 1), SLOW_POLL_MS);
  assert.equal(pollDelay(POLL_LIMIT_MS), null);
  assert.equal(pollDelay(Number.NaN), null);
});

test('202: one POST, then read-only polling through copying and opening to opened', async () => {
  const { controller, timers, calls } = harness([
    listing(op('checking')),
    listing(op('copying', { confirmedCount: 0, totalCount: 18 })),
    listing(op('copying', { confirmedCount: 3, totalCount: 18 })),
    listing(op('opening')),
    listing(op('opened')),
  ]);
  assert.equal(await controller.start('party', 'mac'), 'started');
  assert.equal(controller.running('party'), true);
  assert.equal(controller.views().get('party').text, 'Checking history on Mac');
  const seen = [];
  for (let i = 0; i < 5; i++) { await timers.run(); seen.push(controller.views().get('party')?.text); }
  assert.deepEqual(seen, ['Checking history on Mac', 'Copying history 0 of 18', 'Copying history 3 of 18', 'Opening on Mac', 'Opened on Mac']);
  assert.equal(calls.post, 1);
  assert.equal(calls.list, 5);
  assert.equal(controller.running('party'), false);
  assert.deepEqual(calls.finished, [{ id: 'party', platform: 'mac', state: 'opened', text: 'Opened on Mac' }]);
  // the last line stays briefly, then the row returns to normal; nothing else is polled
  assert.equal(timers.pending(), 1);
  await timers.run();
  assert.equal(controller.views().has('party'), false);
  assert.equal(timers.pending(), 0);
  assert.equal(calls.post, 1);
});

test('202: a failure or an unconfirmed history copy ends with the server sentence, without another POST', async () => {
  for (const [state, message] of [['failed', 'Claude desktop request failed.'], ['blocked_uncertain', OPEN_SENTENCES.blocked_uncertain]]) {
    const { controller, timers, calls } = harness([listing(op('copying', { confirmedCount: 1, totalCount: 4 })), listing(op(state, { message }))]);
    await controller.start('party', 'mac');
    await timers.run(); await timers.run();
    assert.equal(calls.post, 1);
    assert.equal(calls.finished.length, 1);
    assert.equal(calls.finished[0].state, state);
    assert.equal(calls.finished[0].text, message);
    assert.equal(controller.views().get('party').text, message);
  }
});

test('a failed GET while polling is ignored; the POST is never replayed', async () => {
  const { controller, timers, calls } = harness([new Error('offline'), new Error('offline'), listing(op('opened'))]);
  await controller.start('party', 'mac');
  await timers.run(); await timers.run();
  assert.equal(controller.running('party'), true);
  await timers.run();
  assert.equal(controller.running('party'), false);
  assert.equal(calls.post, 1);
  assert.equal(calls.list, 3);
});

test('an operation the server no longer reports ends the progress (no resume after a restart)', async () => {
  const { controller, timers, calls } = harness([listing(op('copying')), listing(null)]);
  await controller.start('party', 'mac');
  await timers.run(); await timers.run();
  assert.equal(calls.finished[0].state, 'lost');
  assert.equal(calls.finished[0].text, OPEN_SENTENCES.lost);
  assert.equal(calls.post, 1);
});

test('another operation of the same profile keeps the poller waiting for its own', async () => {
  const { controller, timers, calls } = harness([listing(op('copying', { id: 'op-2', platform: 'windows' })), listing(op('opened'))]);
  await controller.start('party', 'mac');
  await timers.run();
  assert.equal(controller.views().get('party').text, 'Checking history on Mac');
  await timers.run();
  assert.equal(calls.finished[0].state, 'opened');
});

test('the poller stops after its limit and says the progress shows on the next refresh', async () => {
  const { controller, timers, calls } = harness([listing(op('copying', { confirmedCount: 1, totalCount: 900 }))]);
  await controller.start('party', 'mac');
  let ticks = 0;
  while (controller.running('party') && ticks < 1000) { await timers.run(); ticks++; }
  assert.equal(calls.finished[0].state, 'timeout');
  assert.equal(calls.finished[0].text, OPEN_SENTENCES.timeout);
  // 120 polls at 1 s, then 5 s polls until the 10-minute limit
  assert.equal(calls.list, 120 + (POLL_LIMIT_MS - FAST_FOR_MS) / SLOW_POLL_MS);
  assert.equal(calls.post, 1);
});

test('200 (no managed history copy, or a server without progress) is a finished Open with no polling', async () => {
  const { controller, timers, calls } = harness([], async () => ({ status: 200, body: { opened: true, id: 'party', platform: 'mac' } }));
  assert.equal(await controller.start('party', 'mac'), 'opened');
  assert.equal(timers.pending(), 0);
  assert.equal(calls.list, 0);
  assert.equal(controller.views().size, 0);
});

test('a refusal from the POST propagates and leaves nothing running', async () => {
  const refused = Object.assign(new Error(OPEN_SENTENCES.blocked_uncertain), { status: 409 });
  const { controller, timers } = harness([], async () => { throw refused; });
  await assert.rejects(controller.start('party', 'mac'), refused);
  assert.equal(controller.running('party'), false);
  assert.equal(timers.pending(), 0);
});

test('a second click while an Open runs sends nothing', async () => {
  const { controller, calls } = harness([listing(op('copying'))]);
  await controller.start('party', 'mac');
  assert.equal(await controller.start('party', 'windows'), 'running');
  assert.equal(calls.post, 1);
});

test('reset stops following without sending anything', async () => {
  const { controller, timers, calls } = harness([listing(op('copying'))]);
  await controller.start('party', 'mac');
  controller.reset();
  assert.equal(timers.pending(), 0);
  assert.equal(controller.views().size, 0);
  assert.equal(calls.post, 1);
});

test('running Opens reported by the profile list show on rows; finished ones from before a reload do not', () => {
  const ours = new Map([['gmail', { platform: 'mac', state: 'opening', text: 'Opening on Mac', done: false }]]);
  const profiles = [
    { id: 'gmail', openOperation: op('failed', { id: 'old' }) },
    { id: 'party', openOperation: op('copying', { platform: 'windows', confirmedCount: 2, totalCount: 5 }) },
    { id: 'me', openOperation: op('failed', { message: 'Claude desktop request failed.' }) },
    { id: 'plum', openOperation: null },
  ];
  const views = openProgress(ours, profiles);
  assert.equal(views.get('gmail').text, 'Opening on Mac');
  assert.equal(views.get('party').text, 'Copying history 2 of 5');
  assert.equal(views.has('me'), false);
  assert.equal(views.has('plum'), false);
  assert.equal(openProgress(null, null).size, 0);
});

test('Home, Details and Accounts & Settings show the progress line on that Claude row only', () => {
  const now = Date.parse('2026-10-02T12:00:00Z');
  const at = minutes => new Date(now + minutes * 60_000).toISOString();
  const claude = id => ({
    id: `claude:${id}`, provider: 'claude', providerLabel: 'Claude', label: 'Claude', email: `${id}@example.test`, plan: 'pro', platform: 'mac',
    source: 'Native quota', status: 'ok', message: null, fetchedAt: at(-1), sampledAt: at(-1), isActive: false,
    windows: [{ key: 'seven_day', label: 'Weekly usage', kind: 'rate_limit', usedPercent: 10, resetAt: at(600), windowMinutes: 10080 }],
    capabilities: { claudeProfileId: id, claudePlatforms: ['mac', 'windows'] },
  });
  const data = { schemaVersion: 1, updatedAt: at(0), settings: { refreshIntervalSeconds: 60 }, accounts: [claude('party'), claude('gmail')] };
  const progress = new Map([['party', { platform: 'mac', state: 'copying', text: 'Copying history 3 of 18', done: false }]]);
  const rows = dashboardViewModel(data, { now, openProgress: progress }).sections.find(s => s.id === 'claude').rows;
  assert.equal(rows.find(r => r.profile === 'party').meta, 'Copying history 3 of 18');
  assert.equal(rows.find(r => r.profile === 'gmail').meta, 'Pro · 1m ago');
  assert.equal(detailsViewModel(data, 'claude:party', { now, openProgress: progress }).note, 'Copying history 3 of 18');
  const accounts = accountsViewModel(data, { now, openProgress: progress }).colA.find(p => p.id === 'claude').rows;
  assert.equal(accounts.find(r => r.id === 'claude:party').sampled, 'Copying history 3 of 18');
  assert.equal(accounts.find(r => r.id === 'claude:gmail').sampled, 'sampled 1m ago');
  // without progress nothing changes
  assert.equal(dashboardViewModel(data, { now }).sections[0].rows[0].meta, 'Pro · 1m ago');
});
