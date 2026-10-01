import assert from 'node:assert/strict';
import test from 'node:test';
import {filterCookies, projectSample, safeCode, workspaceFromURL} from '../extension/bridge-core.mjs';
const cookie = {name: '__Host-console_session', value: 'synthetic-private', domain: 'opencode.ai', path: '/', secure: true, hostOnly: true, expirationDate: 2000000000};
const sample = {id: 'plan-opencode-go-console-mac-123456abcdef', provider: 'opencode-go', platform: 'mac', status: 'ok', fetchedAt: '2026-10-01T04:00:00Z', sampledAt: '2026-10-01T04:00:00Z', windows: [{key: 'zen-balance', label: 'SECRET', kind: 'balance', remaining: -1.25, unit: 'USD', expiresAt: null}]};
test('only the exact signed-in host and workspace URL is accepted', () => {
  assert.equal(workspaceFromURL('https://opencode.ai/console/org_EXAMPLE/billing'), 'org_EXAMPLE');
  assert.equal(workspaceFromURL('https://opencode.ai/workspace/wrk_EXAMPLE/go'), 'wrk_EXAMPLE');
  for (const url of ['http://opencode.ai/console/org_EXAMPLE', 'https://evil.opencode.ai/console/org_EXAMPLE', 'https://opencode.ai/console/../secret', 'https://opencode.ai/console/']) assert.equal(workspaceFromURL(url), null);
});
test('cookie filtering preserves real expiry and excludes unrelated cookies', () => {
  assert.deepEqual(filterCookies([cookie, {name: 'other', value: 'secret', domain: 'opencode.ai'}], 1900000000), [cookie]);
});
test('expired and missing cookies fail without pretending a zero wallet', () => {
  assert.throws(() => filterCookies([cookie], 2100000000), /no_browser_cookie/);
  assert.throws(() => filterCookies([]), /no_browser_cookie/);
});
test('untrusted domains never enter a native request', () => {
  assert.throws(() => filterCookies([{...cookie, domain: 'evil.opencode.ai'}]), /no_browser_cookie/);
});
test('bad host/path/secure/value/duplicate cookies are rejected', () => {
  for (const change of [{path: '/console'}, {secure: false}, {hostOnly: false}, {value: 'x;token'}, {value: 'x token'}, {expirationDate: NaN}]) assert.throws(() => filterCookies([{...cookie, ...change}]), /invalid_request/);
  assert.throws(() => filterCookies([cookie, cookie]), /invalid_request/);
});
test('native projection keeps signed balances and emits only public fields', () => {
  const projected = projectSample({...sample, cookies: [cookie], label: 'secret-token'});
  assert.equal(projected.windows[0].remaining, -1.25);
  assert.equal(projected.windows[0].label, 'Zen balance');
  assert.equal(projected.label, 'OpenCode console wallet');
  assert.ok(!JSON.stringify(projected).includes('synthetic-private'));
  assert.ok(!JSON.stringify(projected).includes('secret-token'));
});
test('wallet must come from the correct provider/platform', () => {
  for (const change of [{provider: 'claude'}, {platform: 'windows'}, {status: 'error'}, {id: 'wrk_SECRET'}, {fetchedAt: 'tomorrow'}]) assert.throws(() => projectSample({...sample, ...change}), /protocol_error/);
});
test('malformed wallet payloads are rejected', () => {
  for (const change of [{remaining: NaN}, {remaining: '1.0'}, {unit: 'credits'}, {kind: 'rate_limit'}, {key: 'unknown'}]) assert.throws(() => projectSample({...sample, windows: [{...sample.windows[0], ...change}]}), /protocol_error/);
});
test('usage percentages and actual resets remain independent from wallet', () => {
  const row = {key: 'console-week', usedPercent: 37, resetAt: '2026-10-03T04:00:00Z'};
  const projected = projectSample({...sample, windows: [row, ...sample.windows]});
  assert.equal(projected.windows[0].usedPercent, 37);
  assert.equal(projected.windows[0].resetAt, '2026-10-03T04:00:00.000Z');
  assert.equal(projected.windows[1].resetAt, null);
});
test('duplicates, missing wallet and unknown secret errors are rejected', () => {
  assert.throws(() => projectSample({...sample, windows: [...sample.windows, ...sample.windows]}), /protocol_error/);
  assert.throws(() => projectSample({...sample, windows: []}), /protocol_error/);
  assert.equal(safeCode('https://evil/token=SECRET'), 'error');
});
