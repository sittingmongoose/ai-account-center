/**
 * Native fixture-store HTTP contracts and optional warm-route latency/RSS measurement. `--measure` also runs R2's
 * Status measurement under Node on the large fixture (status-measure.mjs) and writes it as `status`.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const repo = fileURLToPath(new URL('../..', import.meta.url));
const worker = process.argv.indexOf('--worker');
if (worker < 0) {
  // The native-store contracts need the fixture store module built on Nas1; a host without it says so instead of
  // failing on an assertion (the same suite then runs unchanged wherever the module exists).
  const storeModule = process.env.AAC_STORE_MODULE ??
    path.join(repo, 'dist/native/aac-store.linux-x64.node');
  if (!fs.existsSync(storeModule)) {
    console.log(
      `[SKIP] read-http-parity: needs the Nas1-built native store at ${storeModule}; ` +
        'copy it into dist/native or set AAC_STORE_MODULE'
    );
    process.exit(0);
  }
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-http-m3-'));
  const reports: any[] = [];
  try {
    for (const mode of process.argv.includes('--ts-only') ? ['off'] : ['off', 'on', 'shadow']) {
      const file = path.join(scratch, mode + '.json');
      const result = spawnSync(
        'bun',
        [
          fileURLToPath(import.meta.url),
          '--worker',
          mode,
          file,
          ...(process.argv.includes('--measure') ? ['--measure'] : []),
        ],
        {
          cwd: repo,
          env: { ...process.env, AAC_NO_LOCAL_RUST_BUILD: '1' },
          encoding: 'utf8',
          maxBuffer: 16 * 1024 * 1024,
        }
      );
      if (result.status !== 0)
        throw new Error(
          (result.stdout || '').slice(-2000) + '\n' + (result.stderr || 'HTTP fixture child failed')
        );
      reports.push(JSON.parse(fs.readFileSync(file, 'utf8')));
      process.stdout.write(result.stdout);
    }
    const file = path.join(repo, 'tests/spec/contract/m3-read-http.json');
    const bytes = JSON.stringify(reports[0].rows, null, 2) + '\n';
    if (process.argv.includes('--regenerate')) fs.writeFileSync(file, bytes);
    else
      assert.ok(fs.readFileSync(file, 'utf8') === bytes, 'Native-store HTTP golden bytes changed');
    for (const report of reports.slice(1))
      assert.ok(
        JSON.stringify(report.rows) === JSON.stringify(reports[0].rows),
        `${report.mode}: HTTP wire contract differs from TS`
      );
    if (process.argv.includes('--measure')) {
      const target = process.env.AAC_M3_MEASURE_OUTPUT;
      assert.ok(target, 'provide AAC_M3_MEASURE_OUTPUT for measurement results');
      // R2: /api/status under Node (the installed runtime) on the large fixture, off, the M3 envelope path and
      // the fed path in ABAB rounds, each its own process over the real Status owner (status-measure.mjs).
      const statusFile = path.join(scratch, 'status.json');
      const measured = spawnSync(
        'node',
        [
          path.join(repo, 'tests/spec/status-measure.mjs'),
          '--dist',
          path.join(repo, 'dist'),
          '--binary',
          process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http'),
          '--out',
          statusFile,
        ],
        { cwd: repo, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 }
      );
      if (measured.status !== 0)
        throw new Error(measured.stderr || measured.stdout || 'Status measurement failed');
      process.stdout.write(measured.stdout);
      fs.writeFileSync(
        target,
        JSON.stringify(
          {
            profile: process.env.AAC_M3_BUILD_PROFILE ?? 'unspecified',
            host: os.hostname(),
            samples: 40,
            kind: 'warm HTTP requests; existing collectors/store writer retained in TS',
            status: JSON.parse(fs.readFileSync(statusFile, 'utf8')),
            reports: reports.map(({ mode, metrics, stats, legacyCalls }) => ({
              mode,
              metrics,
              stats,
              legacyCalls,
            })),
          },
          null,
          2
        ) + '\n'
      );
    }
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
} else {
  const mode = process.argv[worker + 1];
  const destination = process.argv[worker + 2];
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-native-http-'));
  const ccs = path.join(home, '.ccs');
  fs.mkdirSync(ccs, { mode: 0o700 });
  for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
  Object.assign(process.env, {
    CCS_HOME: home,
    CCS_DIR: ccs,
    CCS_DASHBOARD_AUTH_ENABLED: 'true',
    AAC_NO_LOCAL_RUST_BUILD: '1',
  });
  const express = (await import('express')).default;
  const { spyOn } = await import('bun:test');
  const { RustService } = await import('../../src/web-server/rust-service');
  const { createStatusRouter } = await import('../../src/web-server/routes/status-routes');
  const { createScoreboardRouter } = await import('../../src/web-server/routes/scoreboard-routes');
  const { createShareRouter } = await import('../../src/web-server/routes/share-route');
  const { createStoreRouter } = await import('../../src/web-server/routes/store-routes');
  const { apiAuthMiddleware } = await import('../../src/web-server/middleware/auth-middleware');
  const { apiErrorHandler } = await import('../../src/web-server/routes/api-router');
  const { loadNativeStore } = await import('../../src/store/native-loader');
  const { AacStore } = await import('../../src/store/store-service');
  const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
  const { ScoreboardService } = await import('../../src/web-server/scoreboard/scoreboard-service');
  const { ReadersCheckWriter } = await import('../../src/web-server/status/readers-check');
  const statusModule = await import('../../src/web-server/status/status-view');
  const scoreModule = await import('../../src/web-server/scoreboard/scoreboard-view');
  const { writeScoreboardFixture } = await import('../../scripts/scoreboard-fixture.mjs');
  const { NOW, writeReport, threadDoc, tasksDoc, item, writeCard, cardDoc } =
    await import('../unit/web-server/status/status-test-helpers');
  const source = path.join(home, 'reports');
  writeScoreboardFixture(source, { now: NOW });
  writeReport(source, 'vm.fixture', 'thread.json', threadDoc('vm.fixture'));
  writeReport(
    source,
    'vm.fixture',
    'tasks.json',
    tasksDoc('vm.fixture', [item('A', 'landed'), item('B', 'blocked')])
  );
  writeCard(source, cardDoc('vm.fixture', 'question'));
  const mediaDir = path.join(source, 'v1/writers/vm.fixture/media');
  fs.mkdirSync(mediaDir, { recursive: true });
  fs.writeFileSync(
    path.join(mediaDir, 'image.png'),
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4])
  );
  const machine = path.join(home, 'machine');
  fs.mkdirSync(path.join(machine, 'share'), { recursive: true });
  fs.writeFileSync(path.join(machine, 'share/readme.md'), '# Native fixture deliverable\n');
  const native = loadNativeStore({
    file: process.env.AAC_STORE_MODULE ?? path.join(repo, 'dist/native/aac-store.linux-x64.node'),
  });
  assert.ok(native.module, 'a Nas1-built native store module is required, no stub store');
  if (!native.module) throw new Error('native fixture store unavailable');
  const handle = new native.module.NativeStore();
  const directory = path.join(ccs, 'store');
  await handle.call(
    'open',
    JSON.stringify({ dir: directory, cacheBytes: 8 * 1024 * 1024, retentionDays: 0 })
  );
  const store = new AacStore(handle, directory);
  const reader = new AgentReportsReader({ root: () => source, now: () => NOW });
  await reader.scan();
  const none = () => ({ data: null, asOf: null, error: null });
  const inputs: any = {
    now: NOW,
    timeZone: 'UTC',
    reports: reader.snapshot(),
    registry: none(),
    machines: { vm: none(), mac: none(), windows: none(), nas1: none(), alexis: none() },
    browsers: none(),
    simlock: none(),
    usage: null,
    answers: [],
  };
  class Checks extends ReadersCheckWriter {
    pending: Promise<void>[] = [];
    override flush() {
      const task = super.flush();
      this.pending.push(task);
      return task;
    }
  }
  const checks = new Checks({ root: () => source, now: () => NOW });
  const { openStatusHandle } = await import('../../src/status-native/pinned');
  const statusHandle = openStatusHandle({
    file: process.env.AAC_STATUS_MODULE ?? path.join(repo, 'dist/native/aac-status.linux-x64.node'),
  });
  assert.ok(!('unavailable' in statusHandle), 'a Nas1-built native status module is required');
  if ('unavailable' in statusHandle) throw new Error(statusHandle.unavailable);
  const scoreboard = new ScoreboardService({
    root: () => source,
    now: () => NOW,
    timeZone: () => 'UTC',
    native: statusHandle,
    store: () => store,
    readersCheck: checks,
  });
  await scoreboard.view();
  await Promise.all(checks.pending);
  const statusBuilder = statusModule.buildStatusView;
  const scoreBuilder = scoreModule.buildScoreboardView;
  const legacyCalls = { status: 0, scoreboard: 0 };
  const statusSpy = spyOn(statusModule, 'buildStatusView').mockImplementation((input) => {
    legacyCalls.status++;
    return statusBuilder(input);
  });
  const scoreSpy = spyOn(scoreModule, 'buildScoreboardView').mockImplementation((input) => {
    legacyCalls.scoreboard++;
    return scoreBuilder(input);
  });
  const bridge = new RustService(mode);
  const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http');
  await bridge.start(binary);
  if (mode !== 'off') assert.equal(bridge.stats.fallbacks, 0);
  const app = express();
  app.use((req, res, next) => {
    res.locals.rustService = bridge;
    if (req.headers['x-session'])
      Object.assign(req, {
        sessionID: 'fixture-session',
        session: { authenticated: true, epoch: 0 },
      });
    if (req.headers['x-device'])
      Object.assign(req, { auth: { kind: 'device', deviceId: 'fixture-device' } });
    next();
  });
  app.use('/api', apiAuthMiddleware);
  app.use('/api/store', createStoreRouter({ store: () => store, now: () => NOW }));
  app.use(
    '/api/scoreboard',
    createScoreboardRouter(() => scoreboard)
  );
  app.use(
    '/api/status/deliverable',
    createShareRouter(() => machine)
  );
  app.use(
    '/api/status',
    createStatusRouter(
      () =>
        ({
          reader: { root: source },
          readInputs: () => inputs,
          view: () => statusBuilder(inputs),
          answers: {},
        }) as any
    )
  );
  app.use(apiErrorHandler);
  const server = await new Promise<import('node:http').Server>((resolve) => {
    const server = app.listen(0, '127.0.0.1', () => resolve(server));
  });
  try {
    const base = `http://127.0.0.1:${(server.address() as any).port}`;
    const rows: any[] = [];
    const routes = [
      '/api/store/status',
      '/api/status',
      '/api/scoreboard',
      '/api/status/files/vm.fixture/media/image.png',
      '/api/status/deliverable?path=share/readme.md',
    ];
    const normalize = (value: any, key = ''): any => {
      if (typeof value === 'string') {
        let text = value
          .split(home)
          .join('<fixture>')
          .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z/g, '<iso>')
          .replace(/\d{8}T\d{6}Z/g, '<stamp>');
        if (['instanceId', 'eventId', 'id'].includes(key) && /^[a-f0-9]{32}$/.test(text))
          text = '<fixture-id>';
        if (/sha256|hash/i.test(key) && /^[a-f0-9]{64}$/.test(text)) text = '<hash>';
        if (key.endsWith('_ms') && /^\d{13}$/.test(text)) text = '<time-ms>';
        return text;
      }
      if (typeof value === 'number' && value > 1e12 && value < 8.64e15) return '<time-ms>';
      if (Array.isArray(value)) return value.map((v) => normalize(v));
      if (value && typeof value === 'object')
        return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, normalize(v, k)]));
      return value;
    };
    async function request(
      route: string,
      headers: Record<string, string> = { 'x-session': 'yes' }
    ) {
      const response = await fetch(base + route, { headers });
      const bytes = Buffer.from(await response.arrayBuffer());
      return { response, bytes };
    }
    for (const route of routes) {
      const { response, bytes } = await request(route);
      assert.equal(response.status, 200, route);
      const json = response.headers.get('content-type')?.includes('application/json');
      const body = json ? normalize(JSON.parse(bytes.toString('utf8'))) : bytes.toString('base64');
      const serialized = typeof body === 'string' ? body : JSON.stringify(body);
      rows.push({
        route,
        status: response.status,
        headers: Object.fromEntries(
          [
            'content-type',
            'cache-control',
            'etag',
            'vary',
            'content-security-policy',
            'x-content-type-options',
          ]
            .filter((key) => response.headers.has(key))
            .map((key) => [
              key,
              key === 'etag'
                ? response.headers.get(key)!.startsWith('W/')
                  ? 'W/<etag>'
                  : '<etag>'
                : response.headers.get(key),
            ])
        ),
        body,
        wireSha256: createHash('sha256').update(serialized).digest('hex'),
      });
    }
    const etag = (await request('/api/status')).response.headers.get('etag')!;
    assert.equal(
      (await request('/api/status', { 'x-session': 'yes', 'If-None-Match': etag })).response.status,
      304
    );
    const invalid = await request('/api/scoreboard?source=invalid');
    assert.equal(invalid.response.status, 400);
    assert.equal((await request('/api/status', {})).response.status, 401);
    assert.equal(
      (await request('/api/status', { 'x-session': 'yes', 'x-device': 'yes' })).response.status,
      403
    );
    if (mode === 'on') {
      assert.equal(legacyCalls.status, 0, 'on must not construct the TS Status view');
      assert.equal(legacyCalls.scoreboard, 0, 'on must not construct the TS Scoreboard base');
      assert.ok(bridge.stats.served >= 5, 'all five routes must actually use Rust');
    }
    if (mode === 'shadow') {
      for (let i = 0; i < 200 && bridge.stats.shadowed < 5; i++)
        await new Promise((r) => setTimeout(r, 5));
      if (bridge.stats.differences > 0)
        console.log('shadow differences:', JSON.stringify(bridge.diagnostics().routes));
      assert.equal(bridge.stats.differences, 0);
      assert.ok(bridge.stats.shadowed >= 5);
    }
    function rss(pid: number) {
      try {
        const match = fs.readFileSync(`/proc/${pid}/status`, 'utf8').match(/^VmRSS:\s+(\d+) kB/m);
        return match ? Number(match[1]) * 1024 : null;
      } catch {
        return null;
      }
    }
    function rustPid() {
      for (const entry of fs.readdirSync('/proc')) {
        if (!/^\d+$/.test(entry)) continue;
        try {
          const status = fs.readFileSync(`/proc/${entry}/status`, 'utf8');
          if (
            /^Name:\s+aac-http$/m.test(status) &&
            new RegExp(`^PPid:\\s+${process.pid}$`, 'm').test(status)
          )
            return Number(entry);
        } catch {
          /* Another process exited. */
        }
      }
      return null;
    }
    const metrics = [];
    if (process.argv.includes('--measure'))
      for (const route of [...routes, '/api/scoreboard?source=real']) {
        const uncached = route.endsWith('?source=real');
        const prepare = () => {
          if (uncached) (scoreboard as any).cached.clear();
        };
        for (let i = 0; i < 5; i++) {
          prepare();
          await request(route);
        }
        const samples = [];
        for (let i = 0; i < 40; i++) {
          prepare();
          const start = performance.now();
          await request(route);
          samples.push(performance.now() - start);
        }
        samples.sort((a, b) => a - b);
        const pid = rustPid();
        if (mode !== 'off') assert.ok(pid, 'measure the owned Rust child RSS');
        const tsRss = rss(process.pid);
        const rustRss = pid ? rss(pid) : 0;
        metrics.push({
          route,
          composition: uncached ? 'uncached view' : 'normal warm route',
          p50Ms: samples[19],
          p95Ms: samples[37],
          tsRssBytes: tsRss,
          rustRssBytes: rustRss,
          totalRssBytes: tsRss === null || rustRss === null ? null : tsRss + rustRss,
        });
      }
    if (mode === 'shadow') {
      for (let i = 0; i < 200 && (bridge as any).active.size > 0; i++)
        await new Promise((r) => setTimeout(r, 5));
      assert.equal((bridge as any).active.size, 0, 'all measured shadow requests must settle');
      if (bridge.stats.differences > 0)
        console.log('shadow differences:', JSON.stringify(bridge.diagnostics().routes));
      assert.equal(bridge.stats.differences, 0);
    }
    if (mode === 'on') {
      assert.equal(legacyCalls.status, 0, 'measured Status requests must stay in Rust');
      assert.equal(legacyCalls.scoreboard, 0, 'measured uncached Scoreboard must stay in Rust');
    }
    fs.writeFileSync(
      destination,
      JSON.stringify({ mode, rows, metrics, stats: bridge.stats, legacyCalls })
    );
    console.log(
      `Native fixture HTTP ${mode}: five routes match, served=${bridge.stats.served}, shadow=${bridge.stats.shadowed}, TS builders=${JSON.stringify(legacyCalls)}`
    );
  } finally {
    await new Promise<void>((r) => server.close(() => r()));
    await Promise.all(checks.pending);
    bridge.stop();
    await handle.call('close', '{}');
    statusSpy.mockRestore();
    scoreSpy.mockRestore();
    fs.rmSync(home, { recursive: true, force: true });
  }
}
