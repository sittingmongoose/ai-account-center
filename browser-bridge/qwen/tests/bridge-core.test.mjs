import test from 'node:test';
import assert from 'node:assert/strict';
import {filterCookies, cookieDiagnostics, projectSample, safeError} from '../extension/bridge-core.mjs';
const cookie = {name: 'login_qwencloud_ticket', value: 'fixture-only', domain: '.qwencloud.com', path: '/', secure: true, expirationDate: 2000};
const sample = {provider: 'qwen', platform: 'windows', status: 'ok', fetchedAt: '2026-09-30T22:00:00Z', sampledAt: '2026-09-30T22:00:00Z', windows: [{key: 'weekly', usedPercent: 40, resetAt: '2026-10-01T23:00:00Z', accessToken: 'fixture-private'}, {key: 'subscription', expiresAt: '2026-10-30T23:00:00Z'}, {key: 'addon-credits', remaining: 0, limit: 100, used: 100, usedPercent: 100, unit: 'credits', kind: 'balance'}], accessToken: 'fixture-private'};
test('cookie projection only permits the chosen console and removes expired records', () => {
  assert.deepEqual(filterCookies([cookie, {...cookie, domain: '.example.com'}, {...cookie, domain: '.home.qwencloud.com.evil.example'}, {...cookie, name: 'expired', expirationDate: 999}], 'intl', 1000), [cookie]);
  assert.deepEqual(filterCookies([cookie], 'cn', 1000), []);
});
test('exact dotted hosts and non-root cookie paths are retained without widening domains', () => {
  const scoped = {...cookie, domain: '.home.qwencloud.com', path: '/tool'};
  assert.deepEqual(filterCookies([scoped], 'intl', 1000), [scoped]);
  assert.deepEqual(filterCookies([{...scoped, domain: '.other.qwencloud.com'}], 'intl', 1000), []);
  const cn = {...cookie, domain: '.bailian.console.aliyun.com', path: '/cn-beijing'};
  assert.deepEqual(filterCookies([cn], 'cn', 1000), [cn]);
});
test('cookie diagnostics contain only counts and the fixed region', () => {
  const scoped = {...cookie, domain: '.home.qwencloud.com'};
  const output = cookieDiagnostics([cookie, scoped], [scoped], 'intl');
  assert.deepEqual(output, {region:'intl',browserRecords:2,selectedRecords:1,exactConsoleHostRecords:1,exactGatewayHostRecords:0});
  assert.equal(JSON.stringify(output).includes(cookie.value), false);
});
test('duplicates from console and gateway are deduplicated', () => { assert.equal(filterCookies([cookie, cookie], 'intl', 1000).length, 1); });
test('native input rejects header injection and unlimited cookie arrays', () => {
  assert.throws(() => filterCookies([{...cookie, value: 'fixture\r\nInjected: yes'}], 'intl', 1000));
  assert.throws(() => filterCookies([{...cookie, name: 'bad name'}], 'intl', 1000));
  assert.throws(() => filterCookies(Array(401).fill(cookie), 'intl', 1000));
});
test('only normalized metadata can enter extension storage', () => {
  const output = projectSample(sample); assert.equal(JSON.stringify(output).includes('fixture-private'), false);
  assert.equal(output.windows[0].remainingPercent, 60);
  assert.equal(output.windows[1].resetAt, null);
  assert.equal(output.windows[1].expiresAt, '2026-10-30T23:00:00.000Z');
  assert.equal(output.windows[2].remaining, 0);
});
test('unknown quota is preserved and cannot create a fabricated zero', () => {
  const output = projectSample({...sample, windows: [{key: 'weekly', usedPercent: null, resetAt: '2026-10-01T23:00:00Z'}]});
  assert.equal(output.windows[0].usedPercent, null); assert.equal(output.windows[0].remainingPercent, null);
});
test('live-shaped monthly usage and individual pack balances preserve separate expiry', () => {
  const pack = (suffix, used) => ({key:`addon-pack-${suffix}`, label:'fixture-private-label', usedPercent:used / 200,
    used, limit:20000, remaining:20000-used, unit:'credits', kind:'balance',
    resetAt:'2026-10-08T16:00:00Z', expiresAt:'2026-10-08T16:00:00Z'});
  const input = {...sample, windows:[
    {key:'monthly', usedPercent:25, used:45000, limit:180000, remaining:135000, unit:'credits', resetAt:'2026-10-20T16:00:00Z'},
    {key:'subscription', expiresAt:'2026-10-20T16:00:00Z'},
    pack('aaaaaaaaaaaa', 0), pack('bbbbbbbbbbbb', 13000), pack('cccccccccccc', 20000),
    {key:'addon-listed-packs', usedPercent:null, remaining:3, unit:'packs', kind:'balance'},
  ]};
  const output = projectSample(input);
  assert.equal(output.windows.length, 6);
  assert.equal(output.windows[3].remaining, 7000);
  assert.equal(output.windows[3].label, 'Additional credit pack 2');
  assert.equal(output.windows[3].resetAt, null);
  assert.equal(output.windows[3].expiresAt, '2026-10-08T16:00:00.000Z');
  assert.equal(output.windows[5].remaining, 3);
  assert.equal(output.windows[5].usedPercent, null);
  assert.equal(JSON.stringify(output).includes('fixture-private'), false);
});
test('individual pack grammar and bounded inventory reject malformed or excessive samples', () => {
  const make = i => ({key:`addon-pack-${i.toString(16).padStart(12,'0')}`, remaining:1, expiresAt:'2026-10-08T16:00:00Z'});
  assert.equal(projectSample({...sample, windows:Array.from({length:100}, (_,i) => make(i))}).windows.length, 100);
  const complete = {...sample, windows:[...['5h','weekly','monthly','subscription','addon-credits','addon-packs','addon-listed-packs'].map(key=>({key,remaining:1})), ...Array.from({length:100}, (_,i)=>make(i))]};
  const output = projectSample(complete);
  assert.equal(output.windows.length, 107);
  assert.ok(JSON.stringify({ok:true,sample:output}).length < 65536);
  assert.throws(() => projectSample({...complete,windows:[...complete.windows,make(101)]}));
  assert.throws(() => projectSample({...sample, windows:Array.from({length:101}, (_,i) => make(i))}));
  assert.throws(() => projectSample({...sample, windows:[make(1),make(1)]}));
  assert.throws(() => projectSample({...sample, windows:[{...make(1),key:'addon-pack-Bearer-fixture-private'}]}));
  assert.throws(() => projectSample({...sample, windows:[{...make(1),key:'addon-pack-aaaaaaaaaaaa-extra'}]}));
});
test('malformed or empty native samples are rejected', () => {
  assert.throws(() => projectSample({...sample, provider: 'claude'}));
  assert.throws(() => projectSample({...sample, windows: []}));
  assert.throws(() => projectSample({...sample, windows: [{key: 'weekly', usedPercent: NaN}]}));
});

test('explicit normalized percentage points preserve fractions and overage without unit reinterpretation', () => {
  for (const percentage of [0.0125, 1.25, 125]) {
    const output = projectSample({...sample, windows: [{key: 'monthly', usedPercent: percentage,
      used: percentage * 10, limit: 1000, remaining: percentage <= 100 ? 1000 - percentage * 10 : null}]});
    assert.equal(output.windows[0].usedPercent, percentage);
    assert.equal(output.windows[0].used, percentage * 10);
    assert.equal(output.windows[0].remainingPercent, Math.max(0, 100 - percentage));
  }
});

test('only fixed native error codes are shown', () => {
  assert.equal(safeError('timeout'), 'network_error');
  assert.equal(safeError('collector_unavailable'), 'host_unavailable');
  assert.equal(safeError('busy'), 'busy');
  assert.equal(safeError({rawBody: 'fixture-private'}), 'error');
  assert.equal(safeError('Bearer fixture-private'), 'error');
});
