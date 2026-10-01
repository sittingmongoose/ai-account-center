import test, {beforeEach} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
const manifest = JSON.parse(readFileSync(new URL('../extension/manifest.json', import.meta.url), 'utf8'));
let listener;
let cookieChanged;
let response;
let heldReply;
let connections = 0;
let sent = [];
let writes = [];
let queries = [];
let stored = {region: 'intl'};
const sender = {id: 'fixture-extension'};
const freshSample = () => ({provider: 'qwen', platform: 'windows', status: 'ok', fetchedAt: new Date().toISOString(), sampledAt: new Date().toISOString(), windows: [{key: 'monthly', usedPercent: 15}, {key:'addon-pack-aaaaaaaaaaaa', usedPercent:64.68614020609749, used:12937.2280412195, limit:20000, remaining:7062.7719587805, unit:'credits', kind:'balance', expiresAt:'2026-10-08T16:00:00Z'}], cookie: 'fixture-never-store'});
globalThis.chrome = {
  cookies: {async getAll(filter) {
    queries.push(filter);
    const permitted = manifest.host_permissions.includes(`*://${filter.domain}/*`);
    return permitted ? [{name: 'fixture_ticket', value: 'fixture-private-cookie', domain: `.${filter.domain}`, path: '/', secure: false}] : [];
  }, onChanged: {addListener(value) {cookieChanged = value;}}},
  storage: {local: {async get(keys) {return Object.fromEntries(keys.filter(key => Object.hasOwn(stored,key)).map(key => [key,structuredClone(stored[key])]));}, async set(value) {writes.push(structuredClone(value)); Object.assign(stored,structuredClone(value));}}},
  alarms: {async create() {}, onAlarm: {addListener() {}}},
  runtime: {id: sender.id, lastError: undefined, onInstalled: {addListener() {}}, onStartup: {addListener() {}}, onMessage: {addListener(value) {listener = value;}},
    connectNative(name) {
      assert.equal(name, 'com.ccs.qwen_usage_bridge'); connections++;
      let receive;
      return {onMessage: {addListener(value) {receive = value;}}, onDisconnect: {addListener() {}}, disconnect() {},
        postMessage(message) {sent.push(message); if(response === 'held') heldReply = value => receive(value); else queueMicrotask(() => receive(response));}};
    }},
};
await import('../extension/service-worker.mjs');
const request = payload => new Promise(resolve => {assert.equal(listener(payload,sender,resolve),true);});
const collect = () => request({type:'collect'});
beforeEach(() => {stored={region:'intl'}; writes=[]; sent=[]; queries=[]; connections=0; heldReply=null; response={ok:true,sample:freshSample()};});
test('concurrent browser sync requests share one local exchange and persist no cookies', async () => {
  const [a,b] = await Promise.all([collect(),collect()]);
  assert.equal(a.ok,true); assert.deepEqual(a,b); assert.equal(connections,1);
  assert.equal(sent[0].cookies[0].value,'fixture-private-cookie');
  assert.equal(JSON.stringify(writes).includes('fixture-private-cookie'),false);
  assert.equal(JSON.stringify(writes).includes('fixture-never-store'),false);
  assert.deepEqual(queries,[{domain:'qwencloud.com'},{domain:'home.qwencloud.com'},{domain:'cs-data.qwencloud.com'}]);
});
test('cookie permissions cover fixed parent domains without arbitrary hosts or page scripting', () => {
  assert.deepEqual(manifest.host_permissions,['*://qwencloud.com/*','*://home.qwencloud.com/*','*://cs-data.qwencloud.com/*','*://aliyun.com/*','*://bailian.console.aliyun.com/*','*://bailian-cs.console.aliyun.com/*']);
  assert.equal(manifest.permissions.includes('scripting'),false);
  assert.equal(manifest.content_scripts,undefined);
});
test('raw helper errors cannot enter browser storage', async () => {
  response={ok:false,errorCode:'fixture-private-cookie',rawBody:{token:'fixture-private-cookie'}};
  const status=await collect(); assert.equal(status.ok,false); assert.equal(status.errorCode,'error');
  assert.equal(JSON.stringify(writes).includes('fixture-private-cookie'),false);
});
test('only the installed extension can initiate collection', () => {
  assert.equal(listener({type:'collect'},{id:'another-extension'},()=>{throw new Error('must not respond');}),false);
});
test('a transient failure preserves only bounded historical data with the original timestamps and unknown identity', async () => {
  const good=await collect(); response={ok:false,errorCode:'network_error'};
  const failed=await collect();
  assert.equal(failed.ok,false); assert.equal(failed.sample,undefined);
  assert.equal(failed.previousSample.identity,'unknown');
  assert.equal(failed.previousSample.region,'intl');
  assert.equal(failed.previousSample.sample.sampledAt,good.sample.sampledAt);
  assert.equal(failed.previousSample.sample.fetchedAt,good.sample.fetchedAt);
  assert.equal(failed.previousSample.lastSuccessAt,good.lastSyncAt);
  assert.equal(failed.previousSample.sample.windows[1].remaining,7062.7719587805);
  assert.equal(JSON.stringify(stored).includes('fixture-private'),false);
});
test('authentication loss cannot retain previous account usage', async () => {
  await collect(); response={ok:false,errorCode:'needs_sign_in'};
  const failed=await collect();
  assert.equal(failed.ok,false); assert.equal(failed.previousSample,undefined);
});
test('a previous sample older than five minutes is withheld', async () => {
  const old=new Date(Date.now()-6*60*1000).toISOString();
  response={ok:true,sample:{...freshSample(),sampledAt:old,fetchedAt:old}};
  await collect(); response={ok:false,errorCode:'network_error'};
  assert.equal((await collect()).previousSample,undefined);
});
test('a region switch clears all current and previous samples before reopening the popup', async () => {
  await collect(); response={ok:false,errorCode:'network_error'}; await collect();
  await request({type:'region',region:'cn'});
  const state=await request({type:'status'});
  assert.equal(state.region,'cn'); assert.equal(state.status,null);
});
test('a fixed sign-in cookie change invalidates current and previous samples', async () => {
  await collect(); cookieChanged({cookie:{domain:'.qwencloud.com'},removed:true});
  assert.equal((await request({type:'status'})).status,null);
});
test('unrelated domain cookies cannot invalidate the Qwen sample', async () => {
  const good=await collect(); cookieChanged({cookie:{domain:'.qwencloud.com.evil.example'},removed:true});
  assert.deepEqual((await request({type:'status'})).status,good);
});
for(const change of ['region','cookie']) test(`${change} changes fence an in-flight result from storage`,async () => {
  response='held'; const old=collect();
  while(!heldReply) await new Promise(resolve=>setImmediate(resolve));
  if(change==='region') await request({type:'region',region:'cn'});
  else cookieChanged({cookie:{domain:'home.qwencloud.com'},removed:false});
  heldReply({ok:true,sample:freshSample()});
  const result=await old;
  assert.equal(result.ok,false); assert.equal(result.errorCode,'context_changed');
  assert.equal((await request({type:'status'})).status,null);
  response={ok:true,sample:freshSample()};
  const current=await collect(); assert.equal(current.ok,true); assert.equal(current.region,change==='region'?'cn':'intl');
});
