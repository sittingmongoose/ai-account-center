import test from 'node:test';
import assert from 'node:assert/strict';
import { claudeSignInFacts, claudeSignInLine, needToasts, signInsFor } from '../public/claude-signins.mjs';
import { dashboardViewModel, detailsViewModel } from '../public/view-model.mjs';

const NOW = Date.parse('2026-11-03T12:00:00Z');
const H = 3600 * 1000;
const iso = (ms) => new Date(ms).toISOString();
const renewal = (extra = {}) => ({
  runner: 'mac', generatedAt: iso(NOW - 60000), stale: false, renewWithinHours: 96, needsYou: [], hosts: [],
  signIns: [
    { host: 'nas1', profile: 'default', email: 'a@example.com', state: 'ok', signInEndsAt: iso(NOW + 20 * 24 * H), renewFrom: iso(NOW + 16 * 24 * H), autoRenew: true },
    { host: 'mac', profile: 'gmail', email: 'a@example.com', state: 'ok', signInEndsAt: iso(NOW + 10 * 24 * H), renewFrom: iso(NOW + 6 * 24 * H), autoRenew: true, lastResult: 'renewed', lastRenewAt: iso(NOW - H) },
    { host: 'mac', profile: 'other', email: 'b@example.com', state: 'ok', signInEndsAt: iso(NOW + 5 * 24 * H) },
  ],
  ...extra,
});
const data = (extra) => ({ accounts: [{ id: 'c1', provider: 'claude', email: 'A@example.com', status: 'ok', windows: [] }], claudeSignIns: renewal(extra) });

test('no runner, no new words', () => {
  assert.equal(claudeSignInLine({ accounts: [] }, 'a@example.com', NOW), '');
  assert.deepEqual(claudeSignInFacts({}, 'a@example.com', NOW), []);
});

test('an account sees only its own sign-ins, Mac first', () => {
  assert.deepEqual(signInsFor(data(), 'a@example.com').map((r) => r.host), ['mac', 'nas1']);
});

test('the row line names the soonest end, then what needs attention first', () => {
  assert.match(claudeSignInLine(data(), 'a@example.com', NOW), /^Auto-renews · next end /);
  assert.equal(claudeSignInLine(data({ stale: true }), 'a@example.com', NOW), 'Sign-in renewal stopped');
  const needs = { needsYou: [{ email: 'a@example.com', reason: 'captcha', how: 'Solve it.', since: null }] };
  assert.equal(claudeSignInLine(data(needs), 'a@example.com', NOW), 'Sign-in renewal needs you');
});

test('a missing end is said, never invented', () => {
  const d = data({ signIns: [{ host: 'mac', profile: 'gmail', email: 'a@example.com', state: 'ok', signInEndsAt: null }] });
  assert.equal(claudeSignInFacts(d, 'a@example.com', NOW).at(-1).value, 'end not reported');
});

test('Details lists the runner and each computer; Home warns in the Claude header', () => {
  const needs = { needsYou: [{ email: 'a@example.com', reason: 'captcha', how: 'Solve the CAPTCHA.', since: null }] };
  const facts = detailsViewModel(data(needs), 'c1', { now: NOW }).facts.map((f) => f.label);
  assert.deepEqual(facts.slice(-4), ['Needs you', 'Sign-in renewal', 'Sign-in · Mac gmail', 'Sign-in · Nas1']);
  const details = detailsViewModel(data(needs), 'c1', { now: NOW });
  assert.equal(details.facts.find((f) => f.label === 'Needs you').value, 'A CAPTCHA in its Chrome window');
  assert.match(details.note, /Solve the CAPTCHA\./);
  const claude = dashboardViewModel(data(needs), { now: NOW }).sections[0];
  assert.match(claude.meta, /1 sign-in step needs you$/);
  assert.ok(claude.metaRuns.some((r) => r.tone === 'warn'));
});

test('Details says when the browser session that approves renewals ends', () => {
  const d = data({ browserSessions: [{ email: 'a@example.com', signedIn: true, sessionEndsAt: iso(NOW + 28 * 24 * H) }] });
  const fact = claudeSignInFacts(d, 'a@example.com', NOW).find((f) => f.label === 'Browser session');
  assert.match(fact.value, /^Signed in until /);
  const out = data({ browserSessions: [{ email: 'a@example.com', signedIn: false, sessionEndsAt: null }] });
  assert.equal(claudeSignInFacts(out, 'a@example.com', NOW).find((f) => f.label === 'Browser session').value, 'Signed out: the next renewal needs one CAPTCHA');
});

test('each new person-only step raises one notice', () => {
  const seen = new Set();
  const d = data({ needsYou: [{ email: 'a@example.com', reason: 'captcha', how: 'Solve it.', since: 'x' }] });
  assert.equal(needToasts(d, seen).length, 1);
  assert.equal(needToasts(d, seen).length, 0);
});
