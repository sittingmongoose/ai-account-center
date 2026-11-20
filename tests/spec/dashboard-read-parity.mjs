/** Accounts/tray bytes and guards through the real private Rust socket. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { once } from 'node:events';

const require = createRequire(import.meta.url);
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dashboard-parity-')));
process.env.CCS_HOME = root;
process.env.CCS_DIR = root;
process.env.AAC_RUST_GROUP_COLLECTION = 'on';
process.env.AAC_RUST_GROUP_HOME = 'off';
process.env.AAC_RUST_HOME_VIEW = 'off';
process.env.AAC_NO_LOCAL_RUST_BUILD = '1';
const dist = path.resolve('dist');
const load = (file) => require(path.join(dist, file));
const express = require('express');
const { RustService } = load('web-server/rust-service.js');
const { createAccountDashboardRouter } = load('web-server/routes/account-dashboard-routes.js');
const { withPassedResetsAt } = load('web-server/services/account-dashboard-service.js');
const { cases } = JSON.parse(
  fs.readFileSync(new URL('./views/home/cases.json', import.meta.url), 'utf8')
);

const rust = new RustService('on');
const servers = [];
let current = cases[0];
let calls = 0;
const read = async () => {
  calls++;
  return { dashboard: structuredClone(current.dashboard), now: current.ctx.now };
};
async function start(service) {
  const app = express();
  app.use((req, res, next) => {
    if (req.get('x-fixture-auth') === 'tray') {
      req.auth = { kind: 'device', deviceId: 'dev_0123456789abcdef0123456789abcdef' };
    } else if (req.get('x-fixture-auth') === 'session') {
      req.session = { authenticated: true };
      req.sessionID = 'fixture-session';
    }
    res.locals.rustService = service;
    next();
  });
  app.use(
    '/api/accounts',
    createAccountDashboardRouter({
      ...(service ? { readDashboard: read } : {}),
      getDashboard: async () => {
        const input = await read();
        return withPassedResetsAt(input.dashboard, input.now);
      },
      scope: () => root,
    })
  );
  const server = app.listen(0, '127.0.0.1');
  servers.push(server);
  await once(server, 'listening');
  return `http://127.0.0.1:${server.address().port}/api/accounts/dashboard`;
}

function query(url, suffix, auth, headers = {}) {
  // Send precisely the requested conditional headers. Fetch can add no-cache
  // and Pragma to a request with If-None-Match.
  return new Promise((resolve, reject) => {
    const request = http.get(
      url + suffix,
      { headers: { 'x-fixture-auth': auth, ...headers } },
      (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(chunk));
        res.on('error', reject);
        res.on('end', () =>
          resolve({
            status: res.statusCode,
            cache: res.headers['cache-control'] ?? null,
            type: res.headers['content-type'] ?? null,
            etag: res.headers.etag ?? null,
            body: Buffer.concat(chunks).toString('utf8'),
          })
        );
      }
    );
    request.on('error', reject);
  });
}

try {
  const binary = path.resolve(process.argv[2] ?? 'dist/native/aac-http');
  await rust.start(binary);
  assert.ok(rust.feedBoot(root), 'the real Rust service acquired the fixture boot');
  const ts = await start();
  const on = await start(rust);
  const served = () => rust.diagnostics().routes['/api/accounts/dashboard']?.fedServed ?? 0;
  let equal = 0;
  let declined = 0;
  for (const row of cases) {
    current = row;
    const search = `?platform=${row.ctx.platform === 'windows' ? 'windows' : 'mac'}`;
    const before = served();
    const reference = await query(ts, search, 'session');
    const rustReply = await query(on, search, 'session');
    assert.deepEqual(rustReply, reference, row.name);
    if (served() > before) equal++;
    else declined++;
  }
  assert.ok(equal >= 140, `expected Rust to serve most dashboard cases (${equal})`);
  current = cases.find((row) => row.name === 'home-page#1') ?? cases[0];
  const prior = served();
  const tray = await query(on, '?platform=windows', 'tray');
  assert.deepEqual(tray, await query(ts, '?platform=windows', 'tray'));
  assert.ok(served() > prior, 'paired tray request was served by Rust');
  const validators = { 'if-none-match': tray.etag };
  for (const [headers, expected] of [
    [validators, 304],
    [{ ...validators, 'cache-control': 'no-cache' }, 200],
  ]) {
    const conditional = await query(on, '?platform=windows', 'tray', headers);
    assert.equal(conditional.status, expected);
    assert.deepEqual(conditional, await query(ts, '?platform=windows', 'tray', headers));
  }
  const beforeBad = calls;
  for (const [suffix, auth, code] of [
    ['', '', 401],
    ['?platform=linux', 'session', 400],
    ['?refresh=yes', 'tray', 400],
    ['?unused=1', 'session', 400],
    ['?platform=mac&platform=windows', 'tray', 400],
  ]) {
    const actual = await query(on, suffix, auth);
    assert.equal(actual.status, code);
    assert.deepEqual(actual, await query(ts, suffix, auth));
  }
  assert.equal(calls, beforeBad);
  console.log(
    `PASS Accounts/dashboard: ${equal} Rust byte/header matches, ${declined} identical TS fallbacks, tray/304 and five guards`
  );
} finally {
  await rust.stop();
  for (const server of servers) await new Promise((resolve) => server.close(resolve));
  fs.rmSync(root, { recursive: true, force: true });
}
