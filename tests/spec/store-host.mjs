/**
 * R3 store host against the Nas1-built binaries, under Node, through the built bridge and
 * store in dist/. Run after `bun run build:server` with dist/native/aac-http and the
 * aac-store .node module in dist/native. Fixture roots only; the only processes it signals
 * are the service children it started. The exit gate of design 1.3:
 *   bothHandles  the same AacStore calls answer the same through NAPI and the socket;
 *   doubleOpen   while the service holds the store, a NAPI open reads lock_held;
 *   crashRecord / crashActivity / crashBackup / crashMaintain
 *                kill -9 of the service during the call; after the restart verify is clean
 *                and no acknowledged write is lost;
 *   fallback     after the restart budget is used up, TS opens the same files via NAPI;
 *   restore      a backup written by the service restores and verifies;
 *   statusRoute  /api/store/status is answered from the Engine in process.
 *
 *   node tests/spec/store-host.mjs [--ts-only]
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';

if (process.argv.includes('--ts-only')) {
  console.log('store-host: hosting is off in this mode; real store suites run in the off gate');
  process.exit(0);
}

const repo = fileURLToPath(new URL('../..', import.meta.url));
const dist = process.env.AAC_SPEC_DIST ?? path.join(repo, 'dist');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(dist, 'native/aac-http');
const require = createRequire(import.meta.url);
const { RustService } = require(path.join(dist, 'web-server/rust-service.js'));
const { AacStore, aacStoreState, closeAacStore, openAacStore, resetAacStoreForTests } = require(
  path.join(dist, 'store/store-service.js')
);
const { loadNativeStore, nativeModuleName } = require(path.join(dist, 'store/native-loader.js'));

const { requestRustStore, rustStoreConnection } = require(
  path.join(dist, 'web-server/rust-service-transport.js')
);
const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r3-spec-'));
const services = [];
process.env.AAC_RUST_STORE = 'on';
process.env.AAC_STORE_SHARE_BACKUP_DIR = 'off';
process.env.AAC_STORE_MODULE = path.join(dist, 'native', nativeModuleName());
delete process.env.AAC_STORE;

function root(name) {
  const dir = path.join(scratch, name, '.ccs');
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const real = fs.realpathSync(dir);
  process.env.CCS_HOME = path.dirname(real);
  process.env.CCS_DIR = real;
  return real;
}

async function startHosted(name, options) {
  const ccs = root(name);
  const service = new RustService('on', { restartDelaysMs: [100, 100, 100, 100, 100], ...options });
  services.push(service);
  await service.start(binary);
  assert.ok(service.lease, `${name}: the service started`);
  resetAacStoreForTests();
  const opened = await openAacStore();
  assert.equal(opened.hosted, true, `${name}: the store opened hosted`);
  assert.ok(opened.store, `${name}: a store`);
  return { ccs, service, store: opened.store };
}

async function stopHosted(service) {
  await closeAacStore().catch(() => undefined);
  resetAacStoreForTests();
  const socket = service.lease?.connection.socketPath;
  service.stop();
  if (socket) await until(() => !fs.existsSync(socket), 'owned socket cleanup');
  services.splice(services.indexOf(service), 1);
  await until(() => !service.childRssBytes(), 'the owned child stops');
  await new Promise((r) => setTimeout(r, 100));
}

async function until(check, what, ms = 30_000) {
  const end = Date.now() + ms;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > end) throw new Error(`timed out: ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

const T0 = Math.floor(1_790_000_000_000 / 3_600_000) * 3_600_000;
function observation(t, used = 10) {
  return {
    account: {
      identity: 'a'.repeat(64),
      provider: 'codex',
      account_id: 'codex:first',
      plan: null,
      platform: 'ubuntu',
      source: 'r3 spec',
      status: 'ok',
      is_active: true,
    },
    sampled_at_ms: t,
    observed_at_ms: t,
    import_ref: null,
    windows: [
      {
        identity: '["five_hour",null,null]',
        key: 'five_hour',
        label: '5 hour',
        used_pct: used,
        remaining_pct: 100 - used,
        reset_at_ms: t + 3_600_000,
      },
    ],
  };
}

const H = 3_600_000;
const DAY = 86_400_000;
function activityReport(readAt, cells) {
  return {
    schema: 'aac.usage-report.v1',
    computer: 'ubuntu',
    tool: 'codex',
    readAt: new Date(readAt).toISOString(),
    state: 'ok',
    complete: true,
    coverFrom: new Date(Math.floor((readAt - 31 * DAY) / H) * H).toISOString(),
    cells: cells.map(([at, input]) => ({
      hour: new Date(at).toISOString(),
      sessionKey: '0123456789abcdef',
      route: '',
      modelRaw: 'gpt-6',
      inputTokens: input,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      costUsd: 0,
      fallbackCostUsd: 0,
      events: 1,
    })),
    sessions: [],
  };
}

async function napiStore(dir) {
  const loaded = loadNativeStore({
    env: {},
    file: path.join(dist, 'native', nativeModuleName()),
  });
  assert.ok(loaded.module, 'the aac-store module is in dist/native');
  const handle = new loaded.module.NativeStore();
  await handle.call('open', JSON.stringify({ dir, retentionDays: 0 }));
  return new AacStore(handle, dir);
}

function maskedStatus(status) {
  const copy = JSON.parse(JSON.stringify(status));
  copy.instanceId = '<instance>';
  copy.openedAt = 0;
  copy.redbBytes = 0;
  copy.redbStats = {};
  copy.integrity = {};
  copy.findings = [];
  copy.logs = {};
  copy.checkpoints = {};
  copy.meta = {};
  copy.capture = {};
  copy.storageHealth = {};
  return copy;
}

async function battery(store) {
  const out = {};
  for (let i = 0; i < 12; i++) await store.record([observation(T0 + i * 60_000, 10 + i)]);
  out.counts = await store.counts();
  out.oldest = await store.oldest();
  out.history = await store.historyRecords({
    fromMs: 0,
    toMs: T0 + 3_600_000,
    nowMs: T0 + 11 * 60_000,
    thinning: { kind: 'none' },
  });
  out.series = await store.seriesList({ fromMs: 0, toMs: T0 + 3_600_000 });
  out.points = await store.seriesPoints({
    fromMs: T0,
    toMs: T0 + 3_600_000,
    resolution: 'hour',
  });
  out.at = await store.usageAt(T0 + 5 * 60_000);
  out.ingested = await store.activityIngest([
    activityReport(T0 + DAY, [[T0, 100]]),
    { nope: true },
  ]);
  out.activityCounts = await store.activityCounts();
  out.activityOldest = await store.activityOldest();
  out.range = await store.activityRange({ fromMs: T0 - H, toMs: T0 + DAY });
  out.runsStatus = await store.runs('runsStatus');
  out.captured = await store.captureReports([
    {
      kind: 'thread',
      writer: 'r3.spec',
      objectId: 'r3.spec',
      seq: 1,
      writtenAtMs: T0,
      observedAtMs: T0,
      sourceSha256: '0'.repeat(64),
      ids: {},
      doc: { schema: 'agent-reports/thread@1', writer: 'r3.spec', seq: 1, state: 'working' },
    },
  ]);
  out.captureHistory = await store.captureHistory({
    kind: 'thread',
    writer: 'r3.spec',
    objectId: 'r3.spec',
  });
  await store.setMeta('r3-spec', 'yes');
  out.meta = await store.getMeta('ts:r3-spec');
  out.status = maskedStatus(await store.status());
  out.maintained = await store.maintain({ nowMs: T0 + DAY, verify: true, compact: false });
  // Capture ingestion time is the writer's wall clock, rather than a query result.
  const maskIngest = (value) => {
    if (Array.isArray(value)) return value.map(maskIngest);
    if (value && typeof value === 'object')
      return Object.fromEntries(
        Object.entries(value).map(([key, v]) => [
          key,
          key === 'ingestedAtMs' ? '<clock>' : maskIngest(v),
        ])
      );
    return value;
  };
  return maskIngest(out);
}

async function verifyClean(store) {
  const verified = await store.maintain({ verify: true });
  assert.deepEqual(verified.verified.findings, [], 'verify has no findings');
  assert.ok(
    (await store.status()).findings.every((finding) => finding.kind === 'unclean_shutdown'),
    'recovery has no integrity findings (the killed run is disclosed)'
  );
}

async function crashDrill(method, point) {
  const { service, store } = await startHosted(`crash-${method}`, { restartDelaysMs: [500] });
  // Both producers have acknowledged data before every kind of killed operation.
  const acked = [];
  for (let i = 0; i < 12; i++) {
    const sample = observation(T0 + i * 60_000, i);
    await store.record([sample]);
    acked.push(new Date(sample.sampled_at_ms).toISOString());
  }
  const accepted = await store.activityIngest([activityReport(T0 + DAY, [[T0, 100]])]);
  assert.deepEqual(accepted.refused, [], 'baseline activity is acknowledged');
  const before = await store.activityRange({ fromMs: T0 - H, toMs: T0 + DAY });
  assert.ok(before.rows.length > 0, 'baseline activity has a real stored row');
  const arm = path.join(store.directory, `.r3-pause-${point}`);
  const ready = `${arm}.ready`;
  fs.writeFileSync(arm, 'armed');
  let settled = false;
  const pending = (
    method === 'record'
      ? store.record([observation(T0 + 12 * 60_000, 12)])
      : method === 'activityIngest'
        ? store.activityIngest([activityReport(T0 + DAY + 1, [[T0, 200]])])
        : method === 'backup'
          ? store.backup({ roots: [path.join(store.directory, 'backups')], withRedb: true })
          : store.maintain({ nowMs: T0 + DAY, verify: true, compact: true })
  ).then(
    () => {
      settled = true;
    },
    () => {
      settled = true;
    }
  );
  await until(
    () => fs.existsSync(ready),
    `Rust reached ${method}/${point} (requires parity-fixtures)`
  ).catch((error) => {
    fs.rmSync(arm, { force: true });
    fs.rmSync(ready, { force: true });
    throw error;
  });
  assert.equal(settled, false, 'the killed call is in flight at the persistence boundary');
  const boot = service.lease.connection.bootId;
  process.kill(service.lease.pid, 'SIGKILL');
  fs.rmSync(arm, { force: true });
  fs.rmSync(ready, { force: true });
  await pending;
  await until(
    () => service.lease && service.lease.connection.bootId !== boot,
    'the store host restarts'
  );
  const history = await store.historyRecords({
    fromMs: 0,
    toMs: T0 + H,
    nowMs: T0 + 12 * 60_000,
    thinning: { kind: 'none' },
  });
  const sampled = new Set(history.map((row) => row.sampledAt));
  for (const at of acked)
    assert.ok(sampled.has(at), `${method}: acknowledged sample ${at} survived`);
  const after = await store.activityRange({ fromMs: T0 - H, toMs: T0 + DAY });
  assert.ok(
    after.rows.reduce((sum, row) => sum + row[5], 0) >=
      before.rows.reduce((sum, row) => sum + row[5], 0),
    'acknowledged activity survived'
  );
  await verifyClean(store);
  await store.record([observation(T0 + 13 * 60_000, 13)]);
  if (method === 'backup')
    assert.ok(
      (await store.backup({ roots: [path.join(store.directory, 'backups')], withRedb: true })).id
    );
  console.log(
    `store-host drill ${method}: killed in ${point}; ${acked.length} acknowledged samples + activity survived`
  );
  await stopHosted(service);
}

const checks = {
  async pausedReadAndClose() {
    // Max pair 2 M6: a single public read must not wait for unrelated traffic to
    // detect a paused host. Close must finish even while that admitted read waits.
    const { service, store } = await startHosted('paused-read-close', { restartDelaysMs: [1000] });
    await store.status();
    const pid = service.lease.pid;
    process.kill(pid, 'SIGSTOP');
    let read;
    let close;
    try {
      const req = {
        method: 'GET',
        sessionID: 'paused-fixture',
        session: { authenticated: true },
        headers: {},
      };
      read = service
        .compose(
          req,
          '/api/store/status',
          {},
          () => store.status(),
          () => true
        )
        .then(
          () => 'answered',
          (error) => String(error)
        );
      await new Promise((resolve) => setTimeout(resolve, 1100));
      close = closeAacStore();
      const outcome = await Promise.race([
        Promise.all([read, close]).then(([value]) => value),
        new Promise((resolve) => setTimeout(() => resolve('still-waiting'), 3900)),
      ]);
      assert.notEqual(
        outcome,
        'still-waiting',
        'one read and clean close finish before five seconds'
      );
      assert.ok(String(outcome).includes('service_down'), 'the paused read fails honestly');
    } finally {
      try {
        process.kill(pid, 'SIGCONT');
      } catch {
        /* Already reaped by the supervisor. */
      }
      await read;
      await close;
      await stopHosted(service);
    }
  },
  async bothHandles() {
    const napiDir = path.join(scratch, 'napi-store');
    const napi = await napiStore(napiDir);
    const expected = await battery(napi);
    await napi.close();

    const { service, store } = await startHosted('both-handles');
    const hosted = await battery(store);
    await stopHosted(service);
    assert.deepEqual(hosted, expected, 'the socket handle answers as the NAPI handle does');
  },

  async doubleOpen() {
    const { ccs, service, store } = await startHosted('double-open');
    await store.record([observation(T0)]);
    const loaded = loadNativeStore({
      env: {},
      file: path.join(dist, 'native', nativeModuleName()),
    });
    const second = new loaded.module.NativeStore();
    await assert.rejects(
      second.call('open', JSON.stringify({ dir: path.join(ccs, 'store') })),
      /lock_held/,
      'no second writer while the service holds the store'
    );
    await stopHosted(service);
    // After the clean stop the same folder opens in process.
    const reopened = await napiStore(path.join(ccs, 'store'));
    assert.equal(
      (
        await reopened.historyRecords({
          fromMs: 0,
          toMs: T0 + H,
          nowMs: T0,
          thinning: { kind: 'none' },
        })
      ).length,
      1
    );
    await reopened.close();
  },

  async restartRace() {
    const { ccs, service, store } = await startHosted('restart-race', { restartDelaysMs: [1000] });
    await store.record([observation(T0)]);
    const previous = service.lease.connection.bootId;
    process.kill(service.lease.pid, 'SIGKILL');
    await until(() => !service.lease, 'the dead boot retires');
    await assert.rejects(store.counts(), /service_down/);
    const contender = await napiStore(path.join(ccs, 'store'));
    await contender.record([observation(T0 + 60_000)]);
    await until(
      () => service.lease && service.lease.connection.bootId !== previous,
      'restart races the NAPI holder'
    );
    await assert.rejects(
      store.counts(),
      /lock_held/,
      'the restarted service cannot open the competing writer'
    );
    await contender.close();
    assert.equal(
      (
        await store.historyRecords({
          fromMs: 0,
          toMs: T0 + H,
          nowMs: T0 + 60_000,
          thinning: { kind: 'none' },
        })
      ).length,
      2
    );
    const loaded = loadNativeStore({ env: {}, file: process.env.AAC_STORE_MODULE });
    await assert.rejects(
      new loaded.module.NativeStore().call('open', JSON.stringify({ dir: store.directory })),
      /lock_held/
    );
    await verifyClean(store);
    await stopHosted(service);
  },

  async crashRecord() {
    await crashDrill('record', 'append-synced');
  },
  async crashActivity() {
    await crashDrill('activityIngest', 'append-synced');
  },
  async crashBackup() {
    await crashDrill('backup', 'backup-db-closed');
  },
  async crashMaintain() {
    await crashDrill('maintain', 'maintain-sealed');
  },

  async fallback() {
    const { service, store } = await startHosted('fallback', {
      restartDelaysMs: [10, 10, 10, 10, 10],
    });
    await store.record([observation(T0)]);
    for (let kill = 1; kill <= 5; kill++) {
      const current = service.lease;
      assert.ok(current?.pid, `owned child for crash ${kill}`);
      process.kill(current.pid, 'SIGKILL');
      if (kill < 5) {
        await until(
          () => service.lease && service.lease.connection.bootId !== current.connection.bootId,
          `restart ${kill}`
        );
        await store.record([observation(T0 + kill * 60_000)]);
      } else
        await until(
          () => service.diagnostics().service.state === 'gave-up',
          'five crashes exhaust the host budget'
        );
    }
    await until(() => aacStoreState().hosted === false, 'NAPI fallback completes');
    assert.equal(service.lease, null, 'no service writer after fallback');
    await store.record([observation(T0 + 5 * 60_000)]);
    const history = await store.historyRecords({
      fromMs: 0,
      toMs: T0 + H,
      nowMs: T0 + 5 * 60_000,
      thinning: { kind: 'none' },
    });
    assert.equal(history.length, 6, 'all hosted writes and the NAPI write survive');
    await verifyClean(store);
    await stopHosted(service);
  },

  async restore() {
    const { ccs, service, store } = await startHosted('restore');
    for (let i = 0; i < 10; i++) await store.record([observation(T0 + i * 60_000, i)]);
    const before = await store.historyRecords({
      fromMs: 0,
      toMs: T0 + H,
      nowMs: T0 + 9 * 60_000,
      thinning: { kind: 'none' },
    });
    const backup = await store.backup({
      roots: [path.join(ccs, 'store', 'backups')],
      withRedb: true,
    });
    const setDir = path.join(ccs, 'store', 'backups', 'sets', backup.id);
    assert.ok(fs.existsSync(path.join(setDir, 'backup.json')), 'the service wrote the backup');
    await stopHosted(service);

    const loaded = loadNativeStore({
      env: {},
      file: path.join(dist, 'native', nativeModuleName()),
    });
    const restoredDir = path.join(scratch, 'restored-store');
    const restorer = new loaded.module.NativeStore();
    await restorer.call('restore', JSON.stringify({ dir: restoredDir, backupDir: setDir }));
    const restored = await napiStore(restoredDir);
    const verified = await restored.maintain({ verify: true });
    assert.deepEqual(verified.verified.findings, [], 'the restored store verifies');
    const after = await restored.historyRecords({
      fromMs: 0,
      toMs: T0 + H,
      nowMs: T0 + 9 * 60_000,
      thinning: { kind: 'none' },
    });
    assert.deepEqual(after, before, 'the restored history is the backed-up history');
    await restored.close();
  },

  async statusRoute() {
    const { service, store } = await startHosted('status-route');
    await store.record([observation(T0)]);
    const req = {
      method: 'GET',
      originalUrl: '/api/store/status',
      sessionID: 'fixture-session',
      session: { authenticated: true },
      headers: {},
    };
    const servedBefore = service.stats.served;
    const value = await service.compose(
      req,
      '/api/store/status',
      {},
      async () => ({ ts: true }),
      (body) => body?.state === 'ready'
    );
    assert.equal(value.state, 'ready');
    assert.ok(service.stats.served > servedBefore, 'Rust answered from its Engine');
    await stopHosted(service);
  },
};

const suiteFiles = [
  'store-usage',
  'store-activity',
  'store-usage-account-id',
  'store-capture-secret',
].map((name) => `tests/unit/store/${name}.test.ts`);

function suites() {
  for (const handle of ['napi', 'socket']) {
    const env = { ...process.env, AAC_TEST_STORE_HANDLE: handle, AAC_RUST_SERVICE_BINARY: binary };
    delete env.CCS_DIR;
    execFileSync(
      'bun',
      ['test', ...suiteFiles, '--timeout', handle === 'socket' ? '90000' : '30000'],
      {
        cwd: repo,
        env,
        stdio: 'inherit',
      }
    );
    console.log(`store-host existing TS suites: ${handle} passed`);
  }
}

const quantiles = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    n: sorted.length,
    p50Ms: sorted[Math.floor(sorted.length * 0.5)],
    p95Ms: sorted[Math.floor(sorted.length * 0.95)],
  };
};
const fixtureReq = {
  method: 'GET',
  originalUrl: '/api/store/status',
  sessionID: 'fixture-session',
  session: { authenticated: true },
  headers: {},
};

async function seedRuns(store) {
  const writer = 'ubuntu.r3-spec';
  const files = Array.from({ length: 10 }, (_, i) => {
    const ended = T0 + i * 60_000;
    const id = `${writer}--fixture-${i}`;
    const doc = {
      schema: 'agent-reports/run@1',
      id,
      writer,
      seq: 1,
      writtenAt: new Date(ended).toISOString(),
      source: 'observed',
      project: 'r3-fixture',
      stream: null,
      task: { id: `fixture-${i}`, type: 'build', title: 'Fixture build' },
      model: { id: 'openai/gpt-6.1-sol', provider: 'openai', harness: 'codex', effort: 'max' },
      startedAt: new Date(ended - 60_000).toISOString(),
      endedAt: new Date(ended).toISOString(),
      wallSeconds: 60,
      outcome: 'done',
      cost: null,
      scope: {},
      review: null,
      flags: [],
      evidence: [],
    };
    const text = JSON.stringify(doc);
    return {
      path: `writers/${writer}/runs/${doc.endedAt.slice(0, 10)}/${id}.json`,
      text,
      size: Buffer.byteLength(text),
      mtimeMs: ended,
    };
  });
  const result = await store.runs('runsRecord', { files, nowMs: T0 + DAY });
  assert.equal(result.recorded, 10, 'measurement ingests real fixture runs');
  assert.equal((await store.runs('runsQuery')).runs.length, 10, 'measurement query has real rows');
}

async function measureWorker(mode) {
  const loadBefore = os.loadavg();
  let service, store;
  if (mode === 'on') ({ service, store } = await startHosted('measure-on'));
  else {
    const ccs = root('measure-off');
    service = new RustService('off');
    store = await napiStore(path.join(ccs, 'store'));
  }
  // Fixed inputs, identical store cardinality in both modes. Sample record's timestamp changes
  // only within this worker. No source files, provider reads or live collectors are involved.
  for (let i = 0; i < 40; i++) await store.record([observation(T0 + i * 60_000, i)]);
  const query = { fromMs: 0, toMs: T0 + DAY, nowMs: T0 + 40 * 60_000, thinning: { kind: 'none' } };
  await seedRuns(store);
  let tick = 40;
  const calls = {
    record: () => store.record([observation(T0 + tick++ * 60_000, tick % 100)]),
    historyRecords: () => store.historyRecords(query),
    runsQuery: () => store.runs('runsQuery', {}),
  };
  const methodSamples = {};
  for (const [method, call] of Object.entries(calls)) {
    for (let i = 0; i < 10; i++) await call();
    const values = [];
    for (let i = 0; i < 40; i++) {
      const start = performance.now();
      await call();
      values.push(performance.now() - start);
    }
    methodSamples[method] = values;
  }
  const express = require(path.join(repo, 'node_modules/express'));
  const { createStoreRouter } = require(path.join(dist, 'web-server/routes/store-routes.js'));
  const app = express();
  app.use((req, res, next) => {
    req.sessionID = 'fixture-session';
    req.session = { authenticated: true };
    res.locals.rustService = service;
    next();
  });
  app.use('/api/store', createStoreRouter({ store: () => store }));
  const server = app.listen(0, '127.0.0.1');
  await new Promise((resolve) => server.once('listening', resolve));
  const url = `http://127.0.0.1:${server.address().port}/api/store/status`;
  const servedBefore = service.stats.served;
  const httpCall = async () => {
    const response = await fetch(url);
    assert.equal(response.status, 200);
    const body = await response.json();
    assert.equal(body.state, 'ready');
  };
  for (let i = 0; i < 10; i++) await httpCall();
  const routeSamples = [];
  for (let i = 0; i < 40; i++) {
    const start = performance.now();
    await httpCall();
    routeSamples.push(performance.now() - start);
  }
  if (mode === 'on')
    assert.equal(
      service.stats.served - servedBefore,
      50,
      'the real route served every status from Rust'
    );
  global.gc?.();
  const rss = { tsBytes: process.memoryUsage().rss, rustBytes: service.childRssBytes() ?? 0 };
  const result = { mode, methodSamples, routeSamples, rss, loadBefore, loadAfter: os.loadavg() };
  await new Promise((resolve, reject) => server.close((e) => (e ? reject(e) : resolve())));
  if (mode === 'on') await stopHosted(service);
  else {
    await store.close();
    service.stop();
  }
  return result;
}

async function measure() {
  const rounds = [];
  for (let round = 0; round < 5; round++)
    for (const mode of ['off', 'on']) {
      const out = path.join(scratch, `measure-${round}-${mode}.json`);
      execFileSync(
        process.execPath,
        ['--expose-gc', fileURLToPath(import.meta.url), `--measure-worker=${mode}`, `--out=${out}`],
        { cwd: repo, env: process.env, stdio: 'inherit' }
      );
      rounds.push({ round, ...JSON.parse(fs.readFileSync(out, 'utf8')) });
    }
  const rows = {};
  for (const method of ['record', 'historyRecords', 'runsQuery', '/api/store/status']) {
    const samples = (mode) =>
      rounds
        .filter((r) => r.mode === mode)
        .flatMap((r) =>
          method === '/api/store/status' ? r.routeSamples : r.methodSamples[method]
        );
    const off = quantiles(samples('off')),
      on = quantiles(samples('on'));
    const delta = on.p50Ms - off.p50Ms;
    rows[method] = {
      off,
      on,
      deltaP50Ms: delta,
      barMs: method === '/api/store/status' ? 0.5 : 0.3,
      pass: method === '/api/store/status' ? on.p50Ms <= 0.5 : delta <= 0.3,
    };
  }
  const rss = (mode) => {
    const values = rounds
      .filter((r) => r.mode === mode)
      .map((r) => r.rss.tsBytes + r.rss.rustBytes)
      .sort((a, b) => a - b);
    return values[2];
  };
  const rssOff = rss('off'),
    rssOn = rss('on');
  const receipt = loadNativeStore({
    env: {},
    file: process.env.AAC_STORE_MODULE,
  }).module.buildIdentity();
  const result = {
    schema: 'aac.r3.measure.v1',
    measuredAt: new Date().toISOString(),
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    binary,
    identity: JSON.parse(receipt),
    method:
      'Five alternating off/on rounds in fresh Node processes, 10 warm-ups + 40 samples per method and real loopback HTTP route. Fixed 40-point quota fixture plus 10 run documents; no collectors. RSS after GC, TS + owned Rust child; median of rounds.',
    rows,
    rss: {
      offBytes: rssOff,
      onBytes: rssOn,
      deltaMiB: (rssOn - rssOff) / 1048576,
      barMiB: 25,
      pass: rssOn - rssOff <= 25 * 1048576,
    },
    rounds,
  };
  const out = process.argv.find((a) => a.startsWith('--out='))?.slice(6);
  assert.ok(out, '--measure requires --out=/absolute/path');
  fs.writeFileSync(out, JSON.stringify(result, null, 2) + '\n');
  console.log(`store-host measurements written: ${out}`);
}

try {
  const worker = process.argv.find((a) => a.startsWith('--measure-worker='));
  if (worker)
    fs.writeFileSync(
      process.argv.find((a) => a.startsWith('--out=')).slice(6),
      JSON.stringify(await measureWorker(worker.split('=')[1]))
    );
  else if (process.argv.includes('--measure')) await measure();
  else {
    if (!process.argv.includes('--drills-only')) suites();
    let failures = 0;
    const selected = process.argv.find((a) => a.startsWith('--check='))?.slice(8);
    const entries = Object.entries(checks).filter(([name]) => !selected || selected === name);
    assert.ok(entries.length > 0, 'unknown store-host check');
    for (const [name, check] of entries) {
      try {
        await check();
        console.log(`store-host ${name}: ok`);
      } catch (error) {
        failures++;
        console.error(`store-host ${name}: FAIL`, error);
        await closeAacStore().catch(() => undefined);
        resetAacStoreForTests();
        for (const service of services.splice(0)) service.stop();
      }
    }
    assert.equal(failures, 0, `${failures} store host checks failed`);
    console.log(`store-host: ${entries.length} checks passed`);
  }
} finally {
  for (const service of services) service.stop();
  fs.rmSync(scratch, { recursive: true, force: true });
}
