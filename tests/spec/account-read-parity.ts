/**
 * M3 part 3: GET /api/accounts/{visibility,preferences,registry} through the real TS
 * routers over HTTP, in off, on and shadow modes, against the goldens in
 * contract/m3-part3-account-reads.json. On mode must serve every case marked `rust` from the
 * Rust service with no TS reader/composer call, and log a TS fallback for every other case;
 * shadow mode serves TS once per request and finds no difference. Composition tables then
 * compare the TS composers with the Rust socket replies byte for byte.
 *
 *   bun tests/spec/account-read-parity.ts            (off, on, shadow; needs dist/native/aac-http)
 *   bun tests/spec/account-read-parity.ts --ts-only  (off only, no Rust)
 *   ... --regenerate                                 (rewrite the golden from off mode)
 *   ... --measure                                    (add warm latency/RSS samples per route)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const GOLDEN = path.join(repo, 'tests/spec/contract/m3-part3-account-reads.json');
// POSIX file modes are not enforced on Windows, so these two refusals cannot be produced there.
const HOSTLESS =
  process.platform === 'win32'
    ? new Set(['visibility: mode 0644', 'visibility: folder writable by others'])
    : new Set<string>();
const worker = process.argv.indexOf('--worker');

interface Row {
  name: string;
  route: string;
  status: number;
  cacheControl: string | null;
  contentType: string | null;
  body: string;
}
interface CaseStat {
  name: string;
  rust: boolean;
  /** False when shadow mode has nothing to compare: TS threw, or the input never left TS. */
  compared: boolean;
  served: number;
  fallbacks: number;
  shadowed: number;
  differences: number;
  legacy: number;
  effects: number;
}
interface Report {
  mode: string;
  rows: Row[];
  cases: CaseStat[];
  tables: { analytics: TableStat; registry: TableStat };
  zones: { admitted: number; refusedByTs: string[] };
  metrics?: Record<string, { p50: number; p95: number; rssMiB: number }>;
}
interface TableStat {
  total: number;
  served: number;
  refused: string[];
}

if (worker < 0) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m3p3-'));
  const reports: Report[] = [];
  try {
    const modes = process.argv.includes('--ts-only') ? ['off'] : ['off', 'on', 'shadow'];
    for (const mode of modes) {
      const file = path.join(scratch, `${mode}.json`);
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
          maxBuffer: 64 * 1024 * 1024,
        }
      );
      if (result.status !== 0)
        throw new Error(result.stderr || result.stdout || `${mode}: account read worker failed`);
      reports.push(JSON.parse(fs.readFileSync(file, 'utf8')) as Report);
    }
    // Keep the nine original Analytics HTTP replies as references. The public native Analytics
    // M0 run and positive store-backed tests cover the retired Express adapter now.
    const analytics = (row: Row) => row.route.startsWith('/api/accounts/analytics?');
    const golden = JSON.parse(fs.readFileSync(GOLDEN, 'utf8')) as Row[];
    const archivedAnalytics = golden.filter(analytics);
    assert.equal(archivedAnalytics.length, 9, 'the original Analytics reference rows are retained');
    assert.ok(!reports[0].rows.some(analytics), 'the Express route is retired');
    const expected = golden.filter((row) => !HOSTLESS.has(row.name) && !analytics(row));
    const expectedText = JSON.stringify(expected, null, 2) + '\n';
    const bytes = JSON.stringify(reports[0].rows, null, 2) + '\n';
    if (process.argv.includes('--regenerate')) {
      assert.equal(HOSTLESS.size, 0, 'record the goldens on a POSIX host');
      fs.writeFileSync(
        GOLDEN,
        JSON.stringify([...reports[0].rows, ...archivedAnalytics], null, 2) + '\n'
      );
    } else assert.ok(expectedText === bytes, 'account read golden bytes changed');
    for (const report of reports) {
      for (const [index, row] of report.rows.entries())
        assert.deepEqual(
          row,
          reports[0].rows[index],
          `${report.mode}: ${row.name} differs from TS`
        );
      for (const stat of report.cases) {
        const where = `${report.mode}: ${stat.name}`;
        if (report.mode === 'off') {
          assert.equal(stat.served + stat.shadowed, 0, `${where}: off mode asked Rust`);
          assert.equal(stat.legacy, 1, `${where}: off mode TS reader/composer calls`);
        } else if (report.mode === 'on') {
          assert.equal(stat.served, stat.rust ? 1 : 0, `${where}: Rust-served count`);
          assert.equal(stat.legacy, stat.rust ? 0 : 1, `${where}: TS reader/composer calls`);
          if (!stat.rust) assert.ok(stat.fallbacks >= 1, `${where}: fallback not logged`);
          assert.equal(stat.differences, 0, where);
        } else {
          assert.equal(stat.served, 0, `${where}: shadow served Rust`);
          assert.equal(stat.legacy, 1, `${where}: shadow must run TS exactly once`);
          assert.equal(stat.effects, 0, `${where}: shadow applied a Rust-side effect`);
          assert.equal(stat.differences, 0, `${where}: shadow difference`);
          assert.equal(stat.shadowed, stat.compared ? 1 : 0, `${where}: shadow comparison count`);
          if (!stat.rust && stat.compared)
            assert.ok(stat.fallbacks >= 1, `${where}: shadow fallback not logged`);
        }
      }
      if (report.mode !== 'off') {
        for (const [name, table] of Object.entries(report.tables))
          assert.ok(
            table.served > 0 && table.refused.length === 0,
            `${report.mode}: ${name} table ${JSON.stringify(table.refused.slice(0, 5))}`
          );
        assert.deepEqual(report.zones.refusedByTs, [], 'TS refused an admitted zone');
      }
      process.stdout.write(
        `${report.mode}: ${report.rows.length} HTTP cases, ${report.cases.filter((c) => c.rust).length} Rust-eligible; ` +
          `tables analytics ${report.tables.analytics.served}/${report.tables.analytics.total}, ` +
          `registry ${report.tables.registry.served}/${report.tables.registry.total}; zones ${report.zones.admitted}\n`
      );
    }
    if (process.argv.includes('--measure')) {
      const target = process.env.AAC_M3_MEASURE_OUTPUT;
      assert.ok(target, 'provide AAC_M3_MEASURE_OUTPUT for measurement results');
      fs.writeFileSync(
        target,
        JSON.stringify(
          {
            profile: process.env.AAC_M3_BUILD_PROFILE ?? 'unspecified',
            host: os.hostname(),
            kind: 'warm HTTP requests on the fixture; TS owners retained',
            reports: reports.map(({ mode, metrics }) => ({ mode, metrics })),
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
  await runWorker(process.argv[worker + 1], process.argv[worker + 2]);
}

async function runWorker(mode: string, destination: string): Promise<void> {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m3p3-home-'));
  const ccs = path.join(home, '.ccs');
  fs.mkdirSync(ccs, { mode: 0o700 });
  for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
  Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });
  const canonicalCcs = fs.realpathSync(ccs);
  const { spyOn, mock } = await import('bun:test');
  // Fixture homes only: a reader that ignores CCS_HOME must never reach a real private folder.
  const fixtureOs = { ...os, homedir: () => home };
  for (const name of ['os', 'node:os'])
    mock.module(name, () => ({ ...fixtureOs, default: fixtureOs }));
  const guard = (input: unknown) => {
    if (typeof input !== 'string') return;
    const target = path.resolve(input);
    if ([home, repo, os.tmpdir()].some((base) => target === base || target.startsWith(base + '/')))
      return;
    if (
      /\/(?:\.ccs|\.claude|\.codex|\.t3)(?:\/|$)/.test(target) ||
      target.startsWith('/mnt/Cursor/')
    )
      throw Object.assign(new Error('Outside the fixture filesystem'), { code: 'EACCES' });
  };
  const fsPromises = await import('node:fs/promises');
  for (const [object, names, modules] of [
    [
      fs,
      ['readFileSync', 'readdirSync', 'statSync', 'lstatSync', 'openSync', 'existsSync'],
      ['fs', 'node:fs'],
    ],
    [
      fsPromises,
      ['readFile', 'readdir', 'stat', 'lstat', 'open'],
      ['fs/promises', 'node:fs/promises'],
    ],
  ] as const) {
    const guarded: Record<string, unknown> = { ...object };
    for (const name of names) {
      const original = (object as unknown as Record<string, (...a: unknown[]) => unknown>)[name];
      guarded[name] = (input: unknown, ...args: unknown[]) => {
        guard(input);
        return original.call(object, input, ...args);
      };
    }
    for (const name of modules) mock.module(name, () => ({ ...guarded, default: guarded }));
  }
  const express = (await import('express')).default;
  const { RustService } = await import('../../src/web-server/rust-service');
  const { requestRust } = await import('../../src/web-server/rust-service-transport');
  const { createAccountVisibilityRouter } =
    await import('../../src/web-server/routes/account-visibility-routes');
  const { createAccountPreferencesRouter } =
    await import('../../src/web-server/routes/account-preferences-routes');
  const { createAccountLifecycleRouter } =
    await import('../../src/web-server/routes/account-lifecycle-routes');
  const { createAccountRefreshSettingsRouter } =
    await import('../../src/web-server/routes/account-refresh-settings-routes');
  const visibilityModule = await import('../../src/web-server/services/account-visibility');
  const preferencesModule = await import('../../src/web-server/services/dashboard-preferences');
  const listingModule = await import('../../src/web-server/services/account-lifecycle-listing');
  const accountsModule = await import('../../src/web-server/services/account-lifecycle-accounts');
  const composeModule = await import('../../src/web-server/services/account-analytics-compose');
  const storeModule = await import('../../src/web-server/services/private-json-store');
  const rangeModule = await import('../../src/web-server/services/account-analytics-range');
  const { rustAnalyticsInput, rustRegistryInput } =
    await import('../../src/web-server/rust-account-input');
  const fixtures = await import('./account-read-fixtures');

  const bridge = new RustService(mode);
  const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http');
  if (mode !== 'off') {
    assert.ok(fs.existsSync(binary), 'on/shadow need a copied Rust-host aac-http binary');
    await bridge.start(binary);
    assert.equal(bridge.stats.fallbacks, 0, 'Rust service did not start');
  }
  const legacy = { count: 0, effects: 0 };
  const counting = <T extends object>(module: T, name: keyof T) => {
    const original = module[name] as unknown as (...args: unknown[]) => unknown;
    spyOn(module, name as never).mockImplementation(((...args: unknown[]) => {
      legacy.count++;
      return original(...args);
    }) as never);
  };
  counting(visibilityModule, 'readAccountVisibility');
  counting(preferencesModule, 'readDashboardPreferences');
  counting(listingModule, 'composeRegistryListing');
  counting(composeModule, 'composeAccountAnalytics');
  const noteFolder = storeModule.notePrivateStoreFolder;
  spyOn(storeModule, 'notePrivateStoreFolder').mockImplementation(((
    ...args: [string, 'group-writable' | 'symlink']
  ) => {
    legacy.effects++;
    return noteFolder(...args);
  }) as never);

  const registryFixture = fixtures.registryFixture(spyOn, accountsModule);
  const app = express();
  app.use((req, res, next) => {
    res.locals.rustService = bridge;
    if (req.headers['x-session'])
      Object.assign(req, {
        sessionID: 'fixture-session',
        session: { authenticated: true, epoch: 0 },
      });
    next();
  });
  app.use('/api/accounts', createAccountVisibilityRouter());
  app.use('/api/accounts', createAccountPreferencesRouter());
  app.use('/api/accounts', createAccountRefreshSettingsRouter({ onChanged: () => {} }));
  app.use('/api/accounts', createAccountLifecycleRouter({ env: () => registryFixture.env }));
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', () => resolve()));
  const port = (server.address() as { port: number }).port;

  const rows: Row[] = [];
  const cases: CaseStat[] = [];
  const settle = async (before: typeof bridge.stats, forwarded: boolean) => {
    if (mode !== 'shadow' || !forwarded) return;
    for (let attempt = 0; attempt < 400; attempt++) {
      if (bridge.stats.shadowed > before.shadowed) return;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
  };
  const run = async (name: string, route: string, rust: boolean, compared = true) => {
    const before = { ...bridge.stats };
    const calls = { ...legacy };
    const response = await fetch(`http://127.0.0.1:${port}${route}`, {
      headers: { 'x-session': '1' },
    });
    const body = await response.text();
    await settle(before, compared);
    rows.push({
      name,
      route,
      status: response.status,
      cacheControl: response.headers.get('cache-control'),
      contentType: response.headers.get('content-type'),
      body: body.split(home).join('<home>'),
    });
    cases.push({
      name,
      rust,
      compared,
      served: bridge.stats.served - before.served,
      fallbacks: bridge.stats.fallbacks - before.fallbacks,
      shadowed: bridge.stats.shadowed - before.shadowed,
      differences: bridge.stats.differences - before.differences,
      legacy: legacy.count - calls.count,
      effects: legacy.effects - calls.effects,
    });
  };

  try {
    for (const form of fixtures.visibilityForms()) {
      if (HOSTLESS.has(`visibility: ${form.name}`)) {
        console.log(`[SKIP] account-read-parity: visibility: ${form.name} (POSIX file modes)`);
        continue;
      }
      fixtures.resetCcs(ccs);
      form.setup(ccs);
      await run(`visibility: ${form.name}`, '/api/accounts/visibility', form.rust);
    }
    fixtures.resetCcs(ccs);
    for (const form of fixtures.preferencesForms()) {
      fixtures.resetCcs(ccs);
      form.setup(ccs);
      await run(`preferences: ${form.name}`, '/api/accounts/preferences', form.rust);
    }
    fixtures.resetCcs(ccs);
    for (const form of registryFixture.forms) {
      form.setup();
      await run(
        `registry: ${form.name}`,
        '/api/accounts/registry',
        form.rust,
        form.compared ?? true
      );
    }

    // Composition tables over the private socket: TS composer vs Rust, byte for byte.
    const tables = {
      analytics: { total: 0, served: 0, refused: [] as string[] },
      registry: { total: 0, served: 0, refused: [] as string[] },
    };
    const zones = { admitted: 0, refusedByTs: [] as string[] };
    const lease = (
      bridge as unknown as { lease: { connection: Parameters<typeof requestRust>[0] } | null }
    ).lease;
    const principal = { kind: 'session' as const, id: 'a'.repeat(64), role: 'admin' as const };
    const active = new Set<import('node:http').ClientRequest>();
    if (lease) {
      for (const [index, facts] of fixtures.analyticsTable().entries()) {
        tables.analytics.total++;
        const expected = JSON.stringify(composeModule.composeAccountAnalytics(facts.compose));
        const input = rustAnalyticsInput(facts);
        const reply = input
          ? await requestRust(lease.connection, '/api/accounts/analytics', principal, active, input)
          : null;
        if (reply?.status === 200 && JSON.stringify(reply.body) === expected)
          tables.analytics.served++;
        else if (reply?.status === 200)
          assert.fail(`analytics table ${index}: Rust differs from TS`);
        else tables.analytics.refused.push(`analytics ${index}`);
      }
      for (const [index, facts] of fixtures.registryTable().entries()) {
        tables.registry.total++;
        const expected = JSON.stringify(listingModule.composeRegistryListing(facts));
        const input = rustRegistryInput(facts);
        const reply = input
          ? await requestRust(lease.connection, '/api/accounts/registry', principal, active, input)
          : null;
        if (reply?.status === 200 && JSON.stringify(reply.body) === expected)
          tables.registry.served++;
        else if (reply?.status === 200)
          assert.fail(`registry table ${index}: Rust differs from TS`);
        else tables.registry.refused.push(`registry ${index}`);
      }
      // Every zone Rust admits for the saved display zone must be one the running TS accepts.
      for (const zone of fixtures.admittedZones()) {
        zones.admitted++;
        if (!rangeModule.isAccountAnalyticsTimeZone(zone)) zones.refusedByTs.push(zone);
      }
    }

    let metrics: Report['metrics'];
    if (process.argv.includes('--measure')) {
      metrics = {};
      fixtures.resetCcs(ccs);
      fixtures.writeMeasureFiles(ccs);
      const routes: Array<[string, string]> = [
        ['settings', '/api/accounts/settings'],
        ['visibility', '/api/accounts/visibility'],
        ['preferences', '/api/accounts/preferences'],
        ['registry', '/api/accounts/registry'],
      ];
      for (const [name, route] of routes) {
        const samples: number[] = [];
        for (let index = 0; index < 45; index++) {
          const started = performance.now();
          const response = await fetch(`http://127.0.0.1:${port}${route}`, {
            headers: { 'x-session': '1' },
          });
          await response.arrayBuffer();
          if (index >= 5) samples.push(performance.now() - started);
        }
        samples.sort((a, b) => a - b);
        const rss = process.memoryUsage().rss + fixtures.childRss(bridge);
        metrics[name] = {
          p50: Number(samples[Math.floor(samples.length * 0.5)].toFixed(3)),
          p95: Number(samples[Math.floor(samples.length * 0.95)].toFixed(3)),
          rssMiB: Number((rss / 1024 / 1024).toFixed(1)),
        };
      }
      // Composition alone, per generated input: the TS composer in process (off) or the Rust
      // socket round-trip including input serialization and reply parsing (on).
      const timed = async (name: string, run: () => Promise<unknown> | unknown) => {
        const samples: number[] = [];
        for (let index = 0; index < 45; index++) {
          const started = performance.now();
          await run();
          if (index >= 5) samples.push(performance.now() - started);
        }
        samples.sort((a, b) => a - b);
        metrics![name] = {
          p50: Number(samples[Math.floor(samples.length * 0.5)].toFixed(3)),
          p95: Number(samples[Math.floor(samples.length * 0.95)].toFixed(3)),
          rssMiB: Number(
            ((process.memoryUsage().rss + fixtures.childRss(bridge)) / 1024 / 1024).toFixed(1)
          ),
        };
      };
      const analyticsFacts = fixtures.analyticsTable();
      const registryFacts = fixtures.registryTable();
      let next = 0;
      if (mode === 'off') {
        await timed('compose:analytics', () =>
          composeModule.composeAccountAnalytics(
            analyticsFacts[next++ % analyticsFacts.length].compose
          )
        );
        await timed('compose:registry', () =>
          listingModule.composeRegistryListing(registryFacts[next++ % registryFacts.length])
        );
      } else if (lease) {
        await timed('compose:analytics', () =>
          requestRust(
            lease.connection,
            '/api/accounts/analytics',
            principal,
            active,
            rustAnalyticsInput(analyticsFacts[next++ % analyticsFacts.length])
          )
        );
        await timed('compose:registry', () =>
          requestRust(
            lease.connection,
            '/api/accounts/registry',
            principal,
            active,
            rustRegistryInput(registryFacts[next++ % registryFacts.length])
          )
        );
      }
    }
    fs.writeFileSync(
      destination,
      JSON.stringify({
        mode,
        rows,
        cases,
        tables,
        zones,
        ...(metrics ? { metrics } : {}),
      } satisfies Report)
    );
  } finally {
    server.close();
    bridge.stop();
    fs.rmSync(home, { recursive: true, force: true });
  }
  process.exit(0);
}
