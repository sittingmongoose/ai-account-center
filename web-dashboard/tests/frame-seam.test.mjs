// The action kinds the app frame's pages may run (public/frame-seam.mjs PAGE_ACTIONS): every Accounts & Settings change
// and the Analytics controls, never sign-in, the frame's own commands or the retired canvas's reports.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PAGE_ACTIONS } from '../public/frame-seam.mjs';
import { MUTATING_ACTIONS } from '../public/accounts-controller.mjs';

test('the allowlist covers every Accounts & Settings change and never sign-in, the frame\'s commands or the canvas', () => {
  for (const kind of MUTATING_ACTIONS) assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  for (const kind of ['details', 'details-closed', 'launch', 'account-rename',
    'flow-submit', 'flow-copy', 'flow-open-url', 'refuse', 'line-cancel', 'pw-toggle', 'pw-typing', 'refresh-interval', 'update-cancel', 'update-retry',
    'install-app']) {
    assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  }
  for (const kind of ['login', 'setup', 'setup-typing', 'login-overlay', 'login-field-focus', 'login-limit-over', 'probe-rect',
    'content-rect', 'content-cover', 'overlay-open', 'overlay-close', 'navigate', 'navigate-dashboard', 'navigate-analytics',
    'theme', 'refresh', 'update-apps', 'logout', 'auth-recheck', 'analytics-refresh', 'analytics-activity-interval',
    'analytics-metric-key', 'analytics-account', 'analytics-account-id', 'analytics-metric',
    // account switching was removed with its actions
    'activate', 'antigravity-activate', 'activation-confirm', 'activation-cancel', 'automatic', 'threshold',
    'antigravity-automatic', 'antigravity-threshold', 'antigravity-pool', 'antigravity-cooldown']) {
    assert.equal(PAGE_ACTIONS.has(kind), false, `${kind} is not a page action`);
  }
});

test('the Analytics page controls are page actions; its refresh is the frame\'s own Refresh', () => {
  for (const kind of ['analytics-range', 'analytics-custom', 'analytics-provider', 'analytics-split', 'analytics-cache', 'analytics-donut',
    'analytics-cbm-sort', 'analytics-donut-open', 'analytics-heat', 'analytics-focus', 'analytics-compare', 'analytics-group', 'analytics-layout']) {
    assert.ok(PAGE_ACTIONS.has(kind), kind);
  }
  assert.equal(PAGE_ACTIONS.has('analytics-refresh'), false);
});

// Device pass r45: the OpenCode Go row's Remove wallet sent `remove-wallet`, which this list lacked, so the frame
// dropped the tap before the controller ran it. Every kind the Accounts controller handles must be a page action.
test('every action the Accounts controller handles is a page action, so no row control is dropped', () => {
  const source = readFileSync(new URL('../public/accounts-controller.mjs', import.meta.url), 'utf8');
  const body = source.slice(source.indexOf('async function handle(action, value)'), source.indexOf('function reset()'));
  const kinds = [...body.matchAll(/case '([a-z-]+)':/g)].map((m) => m[1]);
  assert.ok(kinds.length > 40, `found ${kinds.length} handled kinds`);
  for (const kind of kinds) assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  assert.ok(PAGE_ACTIONS.has('remove-wallet'));
});
