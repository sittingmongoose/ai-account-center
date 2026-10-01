import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
const manifest = JSON.parse(readFileSync(new URL('../extension/manifest.json', import.meta.url), 'utf8'));
let listener;
let response;
let connections = 0;
let sent = [];
let writes = [];
let queries = [];
const sender = {id: 'fixture-extension'};
const sample = {provider: 'qwen', platform: 'windows', status: 'ok', fetchedAt: '2026-09-30T22:00:00Z', sampledAt: '2026-09-30T22:00:00Z', windows: [{key: 'monthly', usedPercent: 15}], cookie: 'fixture-never-store'};
globalThis.chrome = {
  cookies: {async getAll(filter) {
    queries.push(filter);
    // Chrome checks each cookie's own canonical domain and secure attribute,
    // even when the cookie would be sent to an HTTPS console subdomain.
    const permitted = manifest.host_permissions.some(pattern => pattern === '*://qwencloud.com/*' || pattern === 'http://qwencloud.com/*');
    return permitted ? [{name: 'login_qwencloud_ticket', value: 'fixture-private-cookie', domain: '.qwencloud.com', path: '/', secure: false}] : [];
  }},
  storage: {local: {async get() { return {region: 'intl'}; }, async set(value) { writes.push(value); }}},
  alarms: {async create() {}, onAlarm: {addListener() {}}},
  runtime: {id: sender.id, lastError: undefined, onInstalled: {addListener() {}}, onStartup: {addListener() {}}, onMessage: {addListener(value) { listener = value; }},
    connectNative(name) {
      assert.equal(name, 'com.ccs.qwen_usage_bridge'); connections++;
      let receive;
      return {onMessage: {addListener(value) { receive = value; }}, onDisconnect: {addListener() {}}, disconnect() {},
        postMessage(message) { sent.push(message); queueMicrotask(() => receive(response)); }};
    }},
};
await import('../extension/service-worker.mjs');
function collect() { return new Promise(resolve => { assert.equal(listener({type: 'collect'}, sender, resolve), true); }); }
test('concurrent browser sync requests share one local exchange and persist no cookies', async () => {
  response = {ok: true, sample};
  const [a, b] = await Promise.all([collect(), collect()]);
  assert.equal(a.ok, true); assert.deepEqual(a, b); assert.equal(connections, 1);
  assert.equal(sent[0].cookies[0].value, 'fixture-private-cookie');
  assert.equal(JSON.stringify(writes).includes('fixture-private-cookie'), false);
  assert.equal(JSON.stringify(writes).includes('fixture-never-store'), false);
  assert.equal(sent[0].cookies.length, 1);
  assert.deepEqual(queries, [{domain:'qwencloud.com'},{domain:'home.qwencloud.com'},{domain:'cs-data.qwencloud.com'}]);
});
test('cookie permission covers fixed parent domains and non-secure tickets without arbitrary hosts', () => {
  assert.deepEqual(manifest.host_permissions, ['*://qwencloud.com/*','*://home.qwencloud.com/*','*://cs-data.qwencloud.com/*','*://aliyun.com/*','*://bailian.console.aliyun.com/*','*://bailian-cs.console.aliyun.com/*']);
  assert.equal(manifest.permissions.includes('scripting'), false);
  assert.equal(manifest.content_scripts, undefined);
});
test('raw helper errors cannot enter browser storage', async () => {
  response = {ok: false, errorCode: 'fixture-private-cookie', rawBody: {token: 'fixture-private-cookie'}};
  const status = await collect(); assert.equal(status.ok, false); assert.equal(status.errorCode, 'error');
  assert.equal(JSON.stringify(writes).includes('fixture-private-cookie'), false);
});
test('only the installed extension can initiate collection', () => {
  assert.equal(listener({type: 'collect'}, {id: 'another-extension'}, () => { throw new Error('must not respond'); }), false);
});
