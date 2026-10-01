import test from 'node:test';
import assert from 'node:assert/strict';

const cookie = {name: 'llama_dev_sess', value: 'synthetic-web-session', domain: 'dev.meta.ai', path: '/', secure: true, hostOnly: true};
const sample = {provider: 'muse', platform: 'mac', status: 'ok', email: 'example@example.com', plan: 'Muse Code High Usage',
  fetchedAt: '2026-10-01T14:00:00Z', sampledAt: '2026-10-01T13:00:00Z', windows: [
    {key: 'window', usedPercent: 0, resetAt: null, windowMinutes: 300, used: 0, limit: 1000},
    {key: 'weekly', usedPercent: 6, resetAt: '2026-10-05T00:00:00Z', windowMinutes: 10080, used: 60, limit: 1000},
  ]};
let serial = 0;
async function worker(response = {schemaVersion: 1, ok: true, teamId: '42', sample}) {
  const listeners = {}, alarms = new Map(), requests = [], store = {selectedMuseTeamId: '42', lastMuseSample: sample};
  const event = name => ({addListener: fn => {listeners[name] = fn;}});
  globalThis.chrome = {
    runtime: {id: 'fixed-extension', onMessage: event('message'), onStartup: event('startup'), onInstalled: event('installed'),
      sendNativeMessage: async (host, request) => {requests.push({host, request}); return response;}},
    cookies: {getAll: async request => {assert.equal(request.url, 'https://dev.meta.ai/'); return [cookie];}, onChanged: event('cookie')},
    storage: {local: {get: async () => store, set: async values => Object.assign(store, values)}},
    alarms: {create: async (name, config) => alarms.set(name, config), onAlarm: event('alarm')},
  };
  await import(`../extension/service-worker.mjs?fixture=${++serial}`);
  return {listeners, alarms, requests, store};
}

test('browser startup automatically renews selected Muse session and reinstalls a5 minute alarm', async () => {
  const value = await worker();
  await value.listeners.startup();
  assert.equal(value.requests.length, 1);
  assert.equal(value.requests[0].request.teamId, '42');
  assert.equal(value.requests[0].request.action, 'museSync');
  assert.equal(value.requests[0].request.previousSample.teamId, '42');
  assert.equal(value.requests[0].request.previousSample.sample.sampledAt, new Date(sample.sampledAt).toISOString());
  assert.deepEqual(value.alarms.get('ccs-muse-refresh'), {periodInMinutes: 5});
  assert.equal(JSON.stringify(value.store).includes('synthetic-web-session'), false);
});
test('legacy browser sample transfer sends only the sanitized existing observation', async () => {
  const value = await worker();
  value.store.lastMuseSample = {...sample, api_key: 'sk-private', cookies: [cookie]};
  await value.listeners.startup();
  assert.equal(JSON.stringify(value.requests[0].request.previousSample).includes('sk-private'), false);
  assert.equal(JSON.stringify(value.requests[0].request.previousSample).includes('synthetic-web-session'), false);
  assert.equal(value.requests[0].request.previousSample.sample.sampledAt, new Date(sample.sampledAt).toISOString());
});
test('a different selected team or invalid old sample is never migrated', async () => {
  const value = await worker();
  value.store.lastMuseSample = {...sample, email: 'invalid'};
  await value.listeners.startup();
  assert.equal(value.requests[0].request.previousSample, null);
});
test('ordinary extension installation or reload starts ongoing Muse refresh without a popup click', async () => {
  const value = await worker();
  await value.listeners.installed({reason: 'update'});
  assert.equal(value.requests.length, 1);
});
test('cookie rotation is exact origin and exact names only, with one coalescing persistent alarm', async () => {
  const value = await worker();
  for (const other of [{...cookie, domain: '.google.com'}, {...cookie, name: 'datr'}, {...cookie, domain: 'evil.dev.meta.ai'}]) {
    await value.listeners.cookie({cookie: other, removed: false});
  }
  assert.equal(value.alarms.size, 0);
  await value.listeners.cookie({cookie, removed: true});
  await value.listeners.cookie({cookie: {...cookie, value: 'new-session'}, removed: false});
  assert.equal(value.alarms.size, 1);
  assert.deepEqual(value.alarms.get('ccs-muse-cookie-change'), {delayInMinutes: 0.5});
  assert.equal(value.requests.length, 0);
  await value.listeners.alarm({name: 'ccs-muse-cookie-change'});
  assert.equal(value.requests.length, 1);
});
test('rate limited refresh keeps known last reading and does not loop or discard automatic cadence', async () => {
  const value = await worker({schemaVersion: 1, ok: false, code: 'rate_limited'});
  await value.listeners.startup();
  assert.equal(value.requests.length, 1);
  assert.equal(value.store.lastMuseSample.sampledAt, sample.sampledAt);
  assert.equal(value.store.lastMuseError, 'rate_limited');
  assert.deepEqual(value.alarms.get('ccs-muse-refresh'), {periodInMinutes: 5});
});
test('cached helper sample stays cached with its original observation time in browser storage', async () => {
  const value = await worker({schemaVersion: 1, ok: true, teamId: '42', sample: {...sample, status: 'cached', api_key: 'sk-private'}});
  await value.listeners.startup();
  assert.equal(value.store.lastMuseSample.status, 'cached');
  assert.equal(value.store.lastMuseSample.sampledAt, new Date(sample.sampledAt).toISOString());
  assert.equal(value.store.lastMuseSample.fetchedAt, new Date(sample.fetchedAt).toISOString());
  assert.equal(value.store.lastMuseSample.windows[0].usedPercent, 0);
  assert.equal(JSON.stringify(value.store).includes('sk-private'), false);
});
