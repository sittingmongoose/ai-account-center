// The action kinds the app frame's pages may run (public/frame-seam.mjs PAGE_ACTIONS): every Accounts & Settings change
// and the Analytics controls, never sign-in, the frame's own commands or the retired canvas's reports.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PAGE_ACTIONS } from '../public/frame-seam.mjs';
import { MUTATING_ACTIONS } from '../public/accounts-controller.mjs';

test('the allowlist covers every Accounts & Settings change and never sign-in, the frame\'s commands or the canvas', () => {
  for (const kind of MUTATING_ACTIONS) assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  for (const kind of ['details', 'details-closed', 'launch', 'activate', 'antigravity-activate', 'activation-confirm', 'activation-cancel',
    'flow-submit', 'flow-copy', 'flow-open-url', 'refuse', 'line-cancel', 'pw-toggle', 'pw-typing', 'refresh-interval', 'update-cancel',
    'install-app']) {
    assert.ok(PAGE_ACTIONS.has(kind), `${kind} is a page action`);
  }
  for (const kind of ['login', 'setup', 'setup-typing', 'login-overlay', 'login-field-focus', 'login-limit-over', 'probe-rect',
    'content-rect', 'content-cover', 'overlay-open', 'overlay-close', 'navigate', 'navigate-dashboard', 'navigate-analytics',
    'theme', 'refresh', 'update-apps', 'logout', 'auth-recheck', 'analytics-refresh', 'analytics-activity-interval',
    'analytics-metric-key', 'analytics-account', 'analytics-account-id', 'analytics-metric']) {
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
