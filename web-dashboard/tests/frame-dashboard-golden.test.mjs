// The app frame's dashboard controller (public/frame-dashboard.mjs) against what bridge.js did. bridge.js ran the
// dashboard in the frame's hidden iframe until G5 and in the classic Slint app until Leptos finish (step 9), when it was
// deleted; before that, the four differential scripts it and the controller were driven through were recorded from the
// real bridge.js, framed as `/?content=1`, into fixtures/bridge-golden.json (frozen clock, synthetic data). Each step
// here must give the same server calls (method, path and body, in order) and the same views, toasts and header lines.
// Account switching was removed on 2026-11-14: its steps (Activate, the approval, auto-switch) and the Antigravity
// scenario were dropped, and the kept steps were re-recorded from this controller; every difference from the
// bridge.js recording was a switching field, a view version or the action slot Activate used to take.
import './fixtures/frozen-clock.mjs';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { flush, fixtureServer, loadController, sameOut, sameView, reads } from './fixtures/frame-dashboard-fixture.mjs';

const golden = JSON.parse(readFileSync(new URL('./fixtures/bridge-golden.json', import.meta.url), 'utf8'));

/** One recorded step on the controller: `act <kind> <value>`, `command <action>` or `navigate accounts`. */
async function replay(ctl, label) {
  const [cc, co] = [ctl.calls.length, ctl.out().length];
  const [verb, kind, ...rest] = label.split(' ');
  const value = rest.join(' ');
  if (verb === 'act') await ctl.act(kind, value);
  else if (verb === 'command') await ctl.command(kind);
  else if (verb === 'navigate') { ctl.c.page(kind); await flush(16); }
  else throw new Error(`unknown step ${label}`);
  return { calls: ctl.calls.slice(cc), out: ctl.out().slice(co) };
}

async function sameSteps(ctl, steps) {
  for (const expected of steps) {
    const r = await replay(ctl, expected.label);
    assert.deepEqual(r.calls, expected.calls, `${expected.label}: the same server calls as bridge.js`);
    sameOut(r.out, expected.out, `${expected.label}: the same views, toasts and header line as bridge.js`);
  }
}

test('the golden was recorded from bridge.js on the frozen clock', () => {
  assert.match(golden.recordedFrom, /^public\/bridge\.js at [0-9a-f]{7,40}, framed as \/\?content=1; frozen clock/);
  assert.equal(new Date().toISOString(), '2026-11-14T12:00:00.000Z');
});

test('golden: the start reads what bridge.js read', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(server);
  try {
    assert.deepEqual([...new Set(reads(ctl.calls))].sort(), golden.start.reads, 'the same reads at start');
  } finally { ctl.c.stop(); }
});

test('golden: Details, the interval, a Claude Open, Remove and Update apps', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(server);
  try {
    await sameSteps(ctl, golden.home.steps);
    // nothing reaches the removed switching routes
    assert.equal(ctl.calls.some((c) => /\/activate$|\/auto-switch$/.test(c.path)), false);
  } finally { ctl.c.stop(); }
});

test('golden: Accounts & Settings, its reads on entry and a Remove that asks first', async () => {
  const { server } = fixtureServer();
  const ctl = await loadController(server);
  try {
    const entered = await replay(ctl, 'navigate accounts');
    assert.deepEqual(reads(entered.calls).sort(), golden.accounts.entered.reads, 'entering Accounts reads the same blocks');
    const accountsViews = entered.out.filter((x) => x[0] === 'view' && x[1] === 'accounts').map((x) => x[2]);
    sameView(accountsViews.at(-1), golden.accounts.entered.accountsView, 'the same Accounts view');
    await sameSteps(ctl, golden.accounts.steps);
    // Remove asked once
    assert.equal(ctl.calls.filter((c) => c.path === '/api/accounts/codex%3Aother/remove').length, 1);
  } finally { ctl.c.stop(); }
});
