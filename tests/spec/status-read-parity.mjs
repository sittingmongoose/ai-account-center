#!/usr/bin/env node
/**
 * R2 exit gate, end to end (DESIGN-M3-COMPLETE 1.2): GET /api/status through the real front door modules (built
 * dist/), the real StatusService owner (its reports and quota readers on a fixture share; the process collectors,
 * the pm-mail registry and the pins stubbed, no timers) and the real Rust service. Two servers read the same
 * owner at the same clock: today's handler (the service off) and the fed path (the Status group on). At every
 * step both must answer the same status, headers and bytes, and the fed path must have served:
 *   - the first reads, then requests a millisecond, half a second and a second apart (only the clock crosses);
 *   - a new report written to the share and scanned (one source changes on the real reader);
 *   - a machine reading that changes, a collection that fails, a new registry thread, a quota report rewritten,
 *     the time zone changed (the facts move).
 * Then the lifecycle on the real binary: the ETag revalidates (304); a push during a request answers through
 * today's handler (`stale`) and the fed path serves again; a service killed mid-feed is restarted, pushed again
 * and serves; and a negative control: one changed byte in what the service is fed must show as a difference.
 *
 *   node tests/spec/status-read-parity.mjs [--dist <dist>] [--binary <aac-http>] [--ts-only]
 * Fixture data only, in a temporary CCS home; the processes it starts are its own.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const dist = path.resolve(option('dist', path.join(repo, 'dist')));
const binary = path.resolve(option('binary', path.join(dist, 'native/aac-http')));

const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r2-parity-'));
const home = fs.realpathSync(scratch);
const ccs = path.join(home, '.ccs');
const share = path.join(home, 'share');
fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });
for (const key of Object.keys(process.env))
  if (key.startsWith('CCS_') || key.startsWith('AAC_RUST')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_AGENT_REPORTS_DIR: share,
  AAC_PM_MAIL_REGISTRY_DIR: path.join(home, 'registry'),
  NO_COLOR: '1',
});

const require = createRequire(import.meta.url);
const load = (file) => require(path.join(dist, file));

if (args.includes('--ts-only')) {
  const { rustGroupOn } = load('web-server/rust-service-groups.js');
  const { RustService } = load('web-server/rust-service.js');
  const { statusFedService } = load('web-server/status/status-feed.js');
  // The fixture home has no config.yaml: absent is off.
  assert.equal(rustGroupOn('status', {}), false, 'the Status group is off by default');
  assert.equal(statusFedService({ locals: { rustService: new RustService('off') } }), null);
  assert.equal(statusFedService({ locals: { rustService: new RustService('on') } }), null);
  console.log('status-read-parity: off mode feeds nothing (the Status group is off by default)');
  fs.rmSync(scratch, { recursive: true, force: true });
  process.exit(0);
}
assert.ok(fs.existsSync(binary), `needs the Nas1-built aac-http at ${binary}`);

const express = require(path.join(repo, 'node_modules/express'));
const { RustService } = load('web-server/rust-service.js');
const { createStatusRouter } = load('web-server/routes/status-routes.js');
const { StatusService, setStatusRustService } = load('web-server/status/status-service.js');
const { AgentReportsReader } = load('web-server/status/agent-reports-reader.js');
const { QuotaReportReader } = load('web-server/status/quota-report.js');
const { statusFedService } = load('web-server/status/status-feed.js');
const { pushRustFeedText } = load('web-server/rust-service-transport.js');
const helpers = await import(
  path.join(repo, 'tests/unit/web-server/status/status-test-helpers.ts')
);
// The shared quota fixture (read-view-fixtures.ts imports without extensions, which Node's own loader refuses).
const quotaDocument = (fields = {}) => {
  const made = spawnSync(
    'bun',
    [
      '-e',
      `import { quotaDocument } from './tests/spec/read-view-fixtures'; console.log(JSON.stringify(quotaDocument(${JSON.stringify(fields)})))`,
    ],
    { cwd: repo, encoding: 'utf8' }
  );
  assert.equal(made.status, 0, made.stderr);
  return JSON.parse(made.stdout);
};

const NOW = helpers.NOW;
let clock = NOW;
let zone = 'America/New_York';
let localSample = {
  cores: 8,
  load1: 1.234,
  busyPct: 12,
  memAvailableBytes: 16 * 1024 ** 3,
  memTotalBytes: 32 * 1024 ** 3,
  disk: { mount: '/', freeBytes: 200 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
  cpuTimes: null,
};
let registry = [];
let collectFails = false;

// ---- the share ---------------------------------------------------------------------------------
helpers.writeReport(share, 'vm.fixture', 'thread.json', helpers.threadDoc('vm.fixture'));
helpers.writeReport(
  share,
  'vm.fixture',
  'tasks.json',
  helpers.tasksDoc('vm.fixture', [helpers.item('A', 'landed'), helpers.item('B', 'blocked')])
);
helpers.writeCard(share, helpers.cardDoc('vm.fixture', 'question'));
const quotaFile = path.join(share, 'v1/quota/report.json');
fs.mkdirSync(path.dirname(quotaFile), { recursive: true });
fs.writeFileSync(quotaFile, JSON.stringify(quotaDocument()));

const owner = new StatusService({
  reader: new AgentReportsReader({ root: () => share, now: () => clock }),
  quotaReader: new QuotaReportReader({ root: () => share, now: () => clock }),
  registryReader: {
    scan: async () => ({ threads: structuredClone(registry), problems: [], changed: true }),
  },
  historyStore: () => null,
  run: async () => ({ code: collectFails ? 1 : null, stdout: '', timedOut: false, missing: true }),
  localMachine: () => 'vm',
  readLocalMachine: async () => {
    if (collectFails) throw new Error('Fixture reading failed.');
    return structuredClone(localSample);
  },
  readRemoteMachine: async () => {
    throw new Error('Fixture host unavailable.');
  },
  readRemotePins: async () => '[]',
  timeZone: () => zone,
  now: () => clock,
  notify: () => undefined,
  subscribeUsage: () => () => undefined,
  refreshSeconds: () => 60,
  pinned: {
    view: () => ({ unavailable: 'the pinned threads have not been read yet' }),
    scanStatusDocs: async () => undefined,
    compose: async () => undefined,
    collect: async () => undefined,
    docs: { snapshot: () => ({ docs: [] }) },
  },
  spendNative: null,
});

const services = [];
const servers = [];
async function start(mode, group) {
  process.env.AAC_RUST_GROUP_STATUS = group;
  const s = new RustService(mode);
  if (mode !== 'off') {
    await s.start(binary);
    assert.equal(s.lease?.connection.ccsDir, ccs);
  }
  assert.equal(statusFedService({ locals: { rustService: s } }) === s, group === 'on');
  services.push(s);
  return s;
}
function app(rust) {
  const instance = express();
  instance.use((req, res, next) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-parity-session' });
    res.locals.rustService = rust;
    next();
  });
  instance.use(
    '/api/status',
    createStatusRouter(() => owner)
  );
  return new Promise((resolve) => {
    const server = instance.listen(0, '127.0.0.1', () =>
      resolve(`http://127.0.0.1:${server.address().port}`)
    );
    servers.push(server);
  });
}
function get(base, headers = {}) {
  return new Promise((resolve, reject) => {
    const request = http.get(`${base}/api/status`, { headers }, (response) => {
      const chunks = [];
      response.on('data', (c) => chunks.push(c));
      response.on('end', () =>
        resolve({
          status: response.statusCode,
          headers: response.headers,
          body: Buffer.concat(chunks).toString('utf8'),
        })
      );
    });
    request.on('error', reject);
  });
}
const HEADERS = ['content-type', 'cache-control', 'etag', 'vary', 'content-length'];
const same = (a, b) =>
  a.status === b.status && a.body === b.body && HEADERS.every((h) => a.headers[h] === b.headers[h]);

const report = { steps: [], failures: [] };
let stopOwner;
try {
  const today = await start('off', 'off');
  const fed = await start('on', 'on');
  // R4: with the store host on, the owner forwards this service's owned snapshots (the fed
  // pipeline is unchanged); without it the owner keeps its TypeScript readers.
  setStatusRustService(fed);
  stopOwner = owner.start();
  // Let the first owned refresh (or TS scan) land before the first comparison.
  await new Promise((r) => setTimeout(r, 300));
  let fedServed = 0;
  const fedView = fed.fedView.bind(fed);
  fed.fedView = async (...a) => {
    const reply = await fedView(...a);
    if (reply && typeof reply === 'object') fedServed++;
    return reply;
  };
  const todayUrl = await app(today);
  const fedUrl = await app(fed);

  /** Both servers read the owner at the same clock; the fed path must serve the same bytes and headers. */
  async function step(name, { mustServe = true } = {}) {
    const before = fedServed;
    const a = await get(todayUrl);
    const b = await get(fedUrl);
    const served = fedServed - before === 1;
    report.steps.push({ name, status: b.status, bytes: b.body.length, served });
    if (!same(a, b))
      report.failures.push(`${name}: the fed server answered other bytes or headers`);
    if (mustServe && !served) report.failures.push(`${name}: the fed path did not serve`);
    return b;
  }

  await owner.scanReports();
  await owner.scanRegistry();
  await owner.collect();
  await step('first read');
  for (const ms of [1, 499, 1000]) {
    clock += ms;
    await step(`the clock +${ms} ms`);
  }
  helpers.writeReport(
    share,
    'mac.second',
    'thread.json',
    helpers.threadDoc('mac.second', {
      state: 'blocked',
      writtenAt: new Date(clock - 5000).toISOString(),
    })
  );
  await owner.scanReports();
  await step('a new report scanned');
  localSample = { ...localSample, load1: 0.049999999999999996, busyPct: 99.995 };
  await owner.collect();
  await step('a machine reading changed');
  // Max pair 2 M5: a machine collector can finish before its siblings. Keep the
  // clock fixed so this proves the machine owner invalidates the feed by itself.
  const oldMachine = await get(fedUrl);
  localSample = { ...localSample, load1: 7.8, busyPct: 81 };
  await owner.collectMachines();
  const changedMachine = await step('machine-only collection before sibling collectors');
  assert.notEqual(
    changedMachine.headers.etag,
    oldMachine.headers.etag,
    'machine-only change moves ETag'
  );
  const revalidatedMachine = await get(fedUrl, { 'if-none-match': oldMachine.headers.etag });
  assert.equal(revalidatedMachine.status, 200, 'old machine ETag cannot revalidate');
  assert.equal(
    revalidatedMachine.body,
    changedMachine.body,
    'new machine bytes survive revalidation'
  );

  collectFails = true;
  await owner.collect();
  await step('a collection failed');
  collectFails = false;
  registry = [
    {
      name: 'mail-worker',
      kind: 'codex',
      machine: 'vm',
      about: 'Fixture é task',
      lastSeen: new Date(clock - 60_000).toISOString(),
      readAt: clock - 30_000,
      problem: null,
    },
  ];
  await owner.scanRegistry();
  await step('a registry thread');
  fs.writeFileSync(quotaFile, JSON.stringify(quotaDocument({ seq: 6 })));
  await owner.scanReports();
  await step('the quota report rewritten');
  zone = 'Asia/Kolkata';
  await step('the time zone changed');
  clock += 2_000_000;
  await owner.collect();
  await step('half an hour later');

  // ---- lifecycle on the real binary ----------------------------------------------------------------
  const first = await get(fedUrl);
  const again = await get(fedUrl, { 'if-none-match': first.headers.etag });
  assert.equal(again.status, 304, 'a matching ETag revalidates');
  assert.equal(again.body, '');
  report.etag = 'W/"<sha256>", 304 on If-None-Match';

  // A push during a request: another push lands first with a generation far ahead, so this front door's next
  // request names one the service no longer holds: `stale`, today's handler answers; then the fed path serves.
  const routes = () => fed.diagnostics().routes['/api/status'];
  const ahead = '9000000000000000';
  const held = await fetchHeld(fed, 'status-reports');
  const pushed = await pushRustFeedText(
    fed.lease.connection,
    'status-reports',
    ahead,
    held.data,
    '{}',
    new Set()
  );
  assert.equal(pushed.body.generation, ahead);
  const raced = await step('a push during a request', { mustServe: false });
  assert.ok((routes().declined.stale ?? 0) >= 1, 'counted as stale');
  assert.ok(!report.steps.at(-1).served, "the raced request is today's");
  let servedAgain = 0;
  for (let i = 0; i < 3 && !servedAgain; i++) {
    await step(`after the race ${i + 1}`, { mustServe: false });
    if (report.steps.at(-1).served) servedAgain = i + 1;
  }
  assert.ok(servedAgain, 'served again after the race');
  assert.ok(raced.status === 200);
  report.stale = { declined: routes().declined.stale, servedAgainAfter: servedAgain };

  // A service killed mid-feed: the supervisor restarts it, the new boot holds nothing, the front door pushes again.
  const oldBoot = fed.lease.connection.bootId;
  process.kill(fed.lease.pid ?? 0, 'SIGKILL');
  const deadline = Date.now() + 15_000;
  while ((!fed.lease || fed.lease.connection.bootId === oldBoot) && Date.now() < deadline) {
    await get(fedUrl);
    await new Promise((r) => setTimeout(r, 200));
  }
  assert.ok(fed.lease && fed.lease.connection.bootId !== oldBoot, 'restarted with a new boot');
  let restarted = false;
  for (let i = 0; i < 3 && !restarted; i++) {
    await step(`after the restart ${i + 1}`, { mustServe: false });
    restarted = report.steps.at(-1).served;
  }
  assert.ok(restarted, 'the new boot is pushed again and serves');
  report.restart = 'new boot pushed again and served';

  // Negative control: the facts the service is fed swap AM and PM in every clock word; the fed answer then
  // differs from today's, and the comparison must say so.
  const push = fed.pushFeed.bind(fed);
  fed.pushFeed = (boot, source, generation, data, facts) =>
    push(
      boot,
      source,
      generation,
      data,
      source === 'status-facts'
        ? facts.replace(/ (AM|PM)"/g, (_m, half) => (half === 'AM' ? ' PM"' : ' AM"'))
        : facts
    );
  zone = 'America/Chicago';
  const failures = report.failures.length;
  await step('negative control');
  assert.equal(report.failures.length, failures + 1, 'the parity check catches a changed byte');
  report.failures.pop();
  report.negativeControl = 'one changed clock word: caught';
  fed.pushFeed = push;
  zone = 'America/Denver';
  await step('after the control');

  console.log(JSON.stringify(report, null, 1));
  assert.deepEqual(report.failures, [], 'no served byte differs from today');
  const served = report.steps.filter((s) => s.served).length;
  console.log(
    `[OK] status-read-parity: ${report.steps.length} steps byte-equal to today's handler (${served} served by the fed path), 304, stale race, restart, negative control`
  );
} finally {
  for (const server of servers) server.close();
  stopOwner?.();
  for (const s of services) s.stop();
  await new Promise((r) => setTimeout(r, 300));
  fs.rmSync(scratch, { recursive: true, force: true });
}

/** The data text of `source` as the front door writes it, from the owner's current read. */
async function fetchHeld(_service, source) {
  assert.equal(source, 'status-reports');
  const { rustInputText } = load('web-server/rust-read-input.js');
  return { data: rustInputText({ reports: owner.readInputs().reports }) };
}
