import test from 'node:test';
import assert from 'node:assert/strict';
import {filterMuseCookies, museWindowLabel, projectMuseSample, projectMuseTeams} from '../extension/bridge-core.mjs';

const cookie = {name: 'llama_dev_sess', value: 'synthetic-web-session', domain: 'dev.meta.ai', path: '/', secure: true, hostOnly: true};
const sample = {provider: 'muse', platform: 'mac', status: 'ok', email: 'example@example.com', plan: 'Muse Code High Usage',
  fetchedAt: '2026-10-01T00:00:00Z', sampledAt: '2026-10-01T00:00:00Z', windows: [
    {key: 'window', usedPercent: 0, resetAt: null, windowMinutes: 300, used: 0, limit: 1000},
    {key: 'weekly', usedPercent: 120, resetAt: '2026-10-02T00:00:00Z', windowMinutes: 10080, used: 1200, limit: 1000},
  ]};
test('Muse cookie selection excludes Meta SSO and unrelated sites', () => {
  assert.deepEqual(filterMuseCookies([cookie, {...cookie, domain: '.auth.meta.com', name: 'auth'}, {...cookie, domain: 'evil.dev.meta.ai'}]), [cookie]);
});
test('device credentials cannot become portal cookies', () => {
  for (const value of ['dca:private-device', 'sk-private-inference', 'x;secret', 'x\nsecret']) assert.throws(() => filterMuseCookies([{...cookie, value}]));
});
test('duplicate and expired sessions fail instead of selecting a different account', () => {
  assert.throws(() => filterMuseCookies([cookie, cookie]));
  assert.throws(() => filterMuseCookies([{...cookie, expirationDate: 1}], 2));
});
test('reported overage and unknown idle reset remain accurate', () => {
  const projected = projectMuseSample(sample);
  assert.equal(projected.windows[1].usedPercent, 120);
  assert.equal(projected.windows[1].remainingPercent, 0);
  assert.equal(projected.windows[0].resetAt, null);
  assert.equal(projected.windows[0].unit, 'weighted tokens');
});
test('window labels use the official card names with our cadence, as the collector does', () => {
  assert.deepEqual(projectMuseSample(sample).windows.map(row => row.label), ['Current usage (5-hour)', 'Weekly limit']);
  assert.equal(museWindowLabel('window', 90), 'Current usage (90-minute)');
  for (const minutes of [null, 0, 525601]) assert.equal(museWindowLabel('window', minutes), 'Current usage');
});
test('upstream secret fields never enter extension storage projection', () => {
  const projected = projectMuseSample({...sample, api_key: 'sk-private', cookies: [cookie], windows: sample.windows.map(row => ({...row, secret: 'private'}))});
  assert.equal(JSON.stringify(projected).includes('private'), false);
});
test('unknown or duplicate quota buckets are rejected', () => {
  assert.throws(() => projectMuseSample({...sample, windows: [{...sample.windows[0], key: 'private'}]}));
  assert.throws(() => projectMuseSample({...sample, windows: [sample.windows[0], sample.windows[0]]}));
});
test('only numeric team identifiers and safe labels reach the selector', () => {
  assert.deepEqual(projectMuseTeams([{id: '42', name: 'Personal'}]), [{id: '42', name: 'Personal'}]);
  assert.throws(() => projectMuseTeams([{id: '../secret', name: 'Other'}]));
  assert.throws(() => projectMuseTeams([{id: '42', name: 'dca:private'}]));
});
