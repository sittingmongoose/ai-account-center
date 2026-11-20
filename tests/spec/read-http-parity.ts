/** Native fixture-store HTTP contract and optional warm-route measurement. */
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
  const storeModule =
    process.env.AAC_STORE_MODULE ?? path.join(repo, 'dist/native/aac-store.linux-x64.node');
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
    const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
    const isStoreRow = (row: { route: string }) => row.route === '/api/store/status';
    if (process.argv.includes('--regenerate')) {
      const rows = saved.map((row: { route: string }) =>
        isStoreRow(row) ? reports[0].rows[0] : row
      );
      fs.writeFileSync(file, JSON.stringify(rows, null, 2) + '\n');
    } else
      assert.deepEqual(
        reports[0].rows,
        saved.filter(isStoreRow),
        'Native-store HTTP golden changed'
      );
    for (const report of reports.slice(1))
      assert.ok(
        JSON.stringify(report.rows) === JSON.stringify(reports[0].rows),
        `${report.mode}: HTTP wire contract differs from TS`
      );
    if (process.argv.includes('--measure')) {
      const target = process.env.AAC_M3_MEASURE_OUTPUT;
      assert.ok(target, 'provide AAC_M3_MEASURE_OUTPUT for measurement results');
      fs.writeFileSync(
        target,
        JSON.stringify(
          {
            profile: process.env.AAC_M3_BUILD_PROFILE ?? 'unspecified',
            host: os.hostname(),
            samples: 40,
            kind: 'warm HTTP requests; existing collectors/store writer retained in TS',

            reports: reports.map(({ mode, metrics, stats }) => ({
              mode,
              metrics,
              stats,
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
  const { RustService } = await import('../../src/web-server/rust-service');
  const { createStoreRouter } = await import('../../src/web-server/routes/store-routes');
  const { apiAuthMiddleware } = await import('../../src/web-server/middleware/auth-middleware');
  const { apiErrorHandler } = await import('../../src/web-server/routes/api-router');
  const { loadNativeStore } = await import('../../src/store/native-loader');
  const { AacStore } = await import('../../src/store/store-service');
  const NOW = Date.parse('2026-11-06T16:30:00Z');
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
  app.use(apiErrorHandler);
  const server = await new Promise<import('node:http').Server>((resolve) => {
    const server = app.listen(0, '127.0.0.1', () => resolve(server));
  });
  try {
    const base = `http://127.0.0.1:${(server.address() as any).port}`;
    const rows: any[] = [];
    const routes = ['/api/store/status'];
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
    if (mode === 'on') assert.ok(bridge.stats.served >= 1, 'the store route uses Rust');
    if (mode === 'shadow') {
      for (let i = 0; i < 200 && bridge.stats.shadowed < 1; i++)
        await new Promise((r) => setTimeout(r, 5));
      assert.ok(bridge.stats.shadowed >= 1);
      assert.equal(bridge.stats.differences, 0);
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
      for (const route of routes) {
        for (let i = 0; i < 5; i++) {
          await request(route);
        }
        const samples = [];
        for (let i = 0; i < 40; i++) {
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
          composition: 'normal warm route',
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
    fs.writeFileSync(destination, JSON.stringify({ mode, rows, metrics, stats: bridge.stats }));
    console.log(
      `Native fixture HTTP ${mode}: 1 store route matches, served=${bridge.stats.served}, shadow=${bridge.stats.shadowed}`
    );
  } finally {
    await new Promise<void>((r) => server.close(() => r()));
    bridge.stop();
    await handle.call('close', '{}');
    fs.rmSync(home, { recursive: true, force: true });
  }
}
