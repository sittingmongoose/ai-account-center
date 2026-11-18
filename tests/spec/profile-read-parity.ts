/**
 * M3 part 4: the profile read routes through the real TS routers over HTTP, in off, on and
 * shadow modes, against the goldens in contract/m3-part4-profile-reads.json:
 *
 *   GET /api/claude/desktop-profiles, /api/claude/desktop-profiles/usage,
 *       /api/codex/profiles, /api/codex/profiles/quotas,
 *       /api/antigravity/profiles, /api/antigravity/profiles/quotas,
 *   plus part 3's visibility/preferences files that only a descriptor check can classify.
 *
 * On mode must serve every case marked `rust` from the Rust service with no TS reader or
 * composer call, and log a TS fallback for every other case; shadow mode serves TS once per
 * request and finds no difference. Generated composition tables then compare the TS composers
 * with the Rust socket replies byte for byte (profile-read-tables.ts).
 *
 *   bun tests/spec/profile-read-parity.ts            (off, on, shadow; needs dist/native/aac-http)
 *   bun tests/spec/profile-read-parity.ts --ts-only  (off only, no Rust)
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
const GOLDEN = path.join(repo, 'tests/spec/contract/m3-part4-profile-reads.json');
const worker = process.argv.indexOf('--worker');
/** The Antigravity routes' response instant. */
const NOW = Date.parse('2026-11-15T12:00:00Z');

interface Row {
  name: string;
  route: string;
  role: string;
  status: number;
  cacheControl: string | null;
  contentType: string | null;
  body: string;
}
interface CaseStat {
  name: string;
  rust: boolean;
  compared: boolean;
  reached: boolean;
  served: number;
  fallbacks: number;
  shadowed: number;
  differences: number;
  legacy: number;
}
interface TableStat {
  total: number;
  served: number;
  /** Refused where TS answers an error (it throws). */
  tsErrors: number;
  /** Refused where the input's JavaScript meaning is deliberately TS's (`mayRefuse`). */
  deferred: number;
  refused: string[];
  unexpected: string[];
}
interface Report {
  mode: string;
  rows: Row[];
  cases: CaseStat[];
  tables: Record<string, TableStat>;
  metrics?: Record<string, { p50: number; p95: number; rssMiB: number; rustChildMiB: number }>;
}

if (worker < 0) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m3p4-'));
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
        throw new Error(result.stderr || result.stdout || `${mode}: profile read worker failed`);
      reports.push(JSON.parse(fs.readFileSync(file, 'utf8')) as Report);
    }
    const bytes = JSON.stringify(reports[0].rows, null, 2) + '\n';
    if (process.argv.includes('--regenerate')) fs.writeFileSync(GOLDEN, bytes);
    else assert.ok(fs.readFileSync(GOLDEN, 'utf8') === bytes, 'profile read golden bytes changed');
    for (const report of reports) {
      for (const [index, row] of report.rows.entries())
        assert.deepEqual(
          row,
          reports[0].rows[index],
          `${report.mode}: ${row.name} differs from TS`
        );
      for (const stat of report.cases) {
        const where = `${report.mode}: ${stat.name}`;
        if (!stat.reached) {
          // A failed owner read answers before the bridge: nothing composes, nothing forwards.
          assert.equal(stat.served + stat.shadowed + stat.legacy, 0, `${where}: composed`);
        } else if (report.mode === 'off') {
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
          assert.equal(stat.differences, 0, `${where}: shadow difference`);
          const profile = report.rows[report.cases.indexOf(stat)].route.match(
            /^\/api\/(?:claude\/desktop-profiles|codex\/profiles|antigravity\/profiles)(?:\/|$)/
          );
          assert.equal(
            stat.shadowed,
            !profile && stat.compared ? 1 : 0,
            `${where}: shadow comparison count`
          );
          if (!stat.rust && stat.compared)
            assert.ok(stat.fallbacks >= 1, `${where}: shadow fallback not logged`);
        }
      }
      if (report.mode !== 'off')
        for (const [name, table] of Object.entries(report.tables))
          assert.ok(
            table.served > 0 && table.unexpected.length === 0,
            `${report.mode}: ${name} table ${JSON.stringify(table.unexpected.slice(0, 5))}`
          );
      const tables = Object.entries(report.tables)
        .map(
          ([name, t]) =>
            `${name} ${t.served}/${t.total} (${t.tsErrors} TS errors, ${t.deferred} TS-only)`
        )
        .join(', ');
      process.stdout.write(
        `${report.mode}: ${report.rows.length} HTTP cases, ${report.cases.filter((c) => c.rust).length} Rust-eligible` +
          (tables ? `; tables served/total ${tables}` : '') +
          '\n'
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
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m3p4-home-'));
  const ccs = path.join(home, '.ccs');
  fs.mkdirSync(ccs, { mode: 0o700 });
  for (const key of Object.keys(process.env))
    if (key.startsWith('CCS_') || key === 'CODEX_HOME') delete process.env[key];
  Object.assign(process.env, {
    CCS_HOME: home,
    CCS_DIR: ccs,
    CCS_DASHBOARD_AUTH_ENABLED: 'true',
    AAC_NO_LOCAL_RUST_BUILD: '1',
  });
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
  const claudeDesktopRoutes = (await import('../../src/web-server/routes/claude-desktop-routes'))
    .default;
  const codexRoutes = (await import('../../src/web-server/routes/codex-routes')).default;
  const { createAntigravityRouter } =
    await import('../../src/web-server/routes/antigravity-routes');
  const { createAccountVisibilityRouter } =
    await import('../../src/web-server/routes/account-visibility-routes');
  const { createAccountPreferencesRouter } =
    await import('../../src/web-server/routes/account-preferences-routes');
  const profileService =
    await import('../../src/web-server/services/claude-desktop-profile-service');
  const usageService = await import('../../src/web-server/services/claude-desktop-usage-service');
  const transport = await import('../../src/web-server/services/claude-desktop-transport');
  const codexDashboard = await import('../../src/codex-auth/codex-auth-dashboard-service');
  const quotaService = await import('../../src/web-server/services/codex-profile-quota-service');
  const collector = await import('../../src/web-server/usage/native-quota-collector');
  const renewal = await import('../../src/web-server/services/codex-profile-renewal-service');
  const publicReads = await import('../../src/antigravity/public-reads');
  const visibilityModule = await import('../../src/web-server/services/account-visibility');
  const preferencesModule = await import('../../src/web-server/services/dashboard-preferences');
  const { getClaudeOpenOperations } =
    await import('../../src/web-server/services/claude-open-operations');
  const profileInput = await import('../../src/web-server/rust-profile-input');
  const fixtures = await import('./profile-read-fixtures');
  const { compositionTables } = await import('./profile-read-tables');

  const bridge = new RustService(mode);
  const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http');
  if (mode !== 'off') {
    assert.ok(fs.existsSync(binary), 'on/shadow need a copied Rust-host aac-http binary');
    await bridge.start(binary);
    assert.equal(bridge.stats.fallbacks, 0, 'Rust service did not start');
  }

  let state = fixtures.emptyState();
  // Remote reads, collectors and the runtime are fixture closures; no SSH, provider or login.
  spyOn(transport, 'readClaudeDesktopUsageHistory').mockImplementation((async (launcher: {
    profilePath?: string;
  }) => {
    const value = state.histories.get(launcher.profilePath ?? '');
    if (value === undefined) throw new Error('no fixture history');
    if (value instanceof Error) throw value;
    return value;
  }) as never);
  spyOn(collector, 'getCodexProfileQuotaRows').mockImplementation(
    (async () => state.rows) as never
  );
  spyOn(collector, 'getCachedCodexProfileQuotaRows').mockImplementation(
    (() => state.rows) as never
  );
  spyOn(renewal, 'getCodexProfileRenewalService').mockImplementation((() => ({
    getStatus: async () => ({ profiles: state.renewals }),
  })) as never);
  // Each route's own TS reader/composer. (Bun's spy also sees a module's internal calls, so
  // the quota route's inner summary is not counted against it.)
  const legacy = new Map<string, number>();
  const counting = <T extends object>(module: T, name: keyof T & string) => {
    const original = module[name] as unknown as (...args: unknown[]) => unknown;
    spyOn(module, name as never).mockImplementation(((...args: unknown[]) => {
      legacy.set(name, (legacy.get(name) ?? 0) + 1);
      return original(...args);
    }) as never);
  };
  const composerOf = (route: string): string => {
    const bare = route.split('?')[0];
    const owners: Record<string, string> = {
      '/api/claude/desktop-profiles': 'listClaudeDesktopProfileMetadata',
      '/api/claude/desktop-profiles/usage': 'composeClaudeDesktopUsage',
      '/api/codex/profiles': 'composeCodexAuthProfilesSummary',
      '/api/codex/profiles/quotas': 'composeCodexProfileQuotas',
      '/api/antigravity/profiles': 'publicInventory',
      '/api/antigravity/profiles/quotas': 'publicQuotas',
      '/api/accounts/visibility': 'readAccountVisibility',
      '/api/accounts/preferences': 'readDashboardPreferences',
    };
    assert.ok(owners[bare], bare);
    return owners[bare];
  };
  counting(profileService, 'listClaudeDesktopProfileMetadata');
  counting(usageService, 'composeClaudeDesktopUsage');
  counting(codexDashboard, 'composeCodexAuthProfilesSummary');
  counting(quotaService, 'composeCodexProfileQuotas');
  counting(publicReads, 'publicInventory');
  counting(publicReads, 'publicQuotas');
  counting(visibilityModule, 'readAccountVisibility');
  counting(preferencesModule, 'readDashboardPreferences');

  const app = express();
  app.use((req, res, next) => {
    res.locals.rustService = bridge;
    if (req.headers['x-session'])
      Object.assign(req, {
        sessionID: 'fixture-session',
        session: { authenticated: true, epoch: 0 },
      });
    if (req.headers['x-device'])
      Object.assign(req, {
        auth: {
          kind: 'device',
          deviceId: 'fixture-device',
          platform: 'mac',
          viaPreviousToken: false,
          tokenSha256: 'f'.repeat(64),
        },
      });
    next();
  });
  app.use('/api/claude', claudeDesktopRoutes);
  app.use('/api/codex', codexRoutes);
  app.use(
    '/api/antigravity',
    createAntigravityRouter(
      {
        getInventory: async () => state.inventory as never,
        getAccounts: async () => state.accounts as never,
      },
      () => NOW
    )
  );
  app.use('/api/accounts', createAccountVisibilityRouter());
  app.use('/api/accounts', createAccountPreferencesRouter());
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', () => resolve()));
  const port = (server.address() as { port: number }).port;

  const rows: Row[] = [];
  const cases: CaseStat[] = [];
  const pendingOpens: Array<() => void> = [];
  const settle = async (before: typeof bridge.stats, compared: boolean, route: string) => {
    if (
      mode !== 'shadow' ||
      !compared ||
      /\/api\/(?:claude\/desktop-profiles|codex\/profiles|antigravity\/profiles)/.test(route)
    )
      return;
    for (let attempt = 0; attempt < 400; attempt++) {
      if (bridge.stats.shadowed > before.shadowed) return;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
  };
  const normalize = (body: string): string => {
    let text = body.split(canonicalCcs).join('<ccs>').split(home).join('<home>');
    // The usage answer's own read instant and an Open's random id belong to this run.
    try {
      const fetchedAt = (JSON.parse(body) as { fetchedAt?: unknown } | null)?.fetchedAt;
      if (typeof fetchedAt === 'string') text = text.split(fetchedAt).join('<fetched-at>');
    } catch {
      /* Not JSON. */
    }
    return text.replace(/"op_[0-9a-f]{24}"/g, '"op_<id>"');
  };
  const run = async (
    name: string,
    route: string,
    rust: boolean,
    compared = true,
    role: 'session' | 'device' = 'session',
    reached = true
  ) => {
    const before = { ...bridge.stats };
    const composer = composerOf(route);
    const calls = legacy.get(composer) ?? 0;
    const response = await fetch(`http://127.0.0.1:${port}${route}`, {
      headers: role === 'device' ? { 'x-device': '1' } : { 'x-session': '1' },
    });
    const body = await response.text();
    await settle(before, compared, route);
    rows.push({
      name,
      route,
      role,
      status: response.status,
      cacheControl: response.headers.get('cache-control'),
      contentType: response.headers.get('content-type'),
      body: normalize(body),
    });
    cases.push({
      name,
      rust,
      compared,
      reached,
      served: bridge.stats.served - before.served,
      fallbacks: bridge.stats.fallbacks - before.fallbacks,
      shadowed: bridge.stats.shadowed - before.shadowed,
      differences: bridge.stats.differences - before.differences,
      legacy: (legacy.get(composer) ?? 0) - calls,
    });
  };
  const prepare = (form: { setup: (ccs: string, home: string, s: typeof state) => void }) => {
    fixtures.resetCcs(ccs);
    fs.rmSync(path.join(home, '.codex'), { recursive: true, force: true });
    state = fixtures.emptyState();
    usageService.invalidateClaudeDesktopUsageCache();
    codexDashboard.invalidateCodexAuthProfilesCache();
    form.setup(ccs, home, state);
    if (state.openProfile)
      getClaudeOpenOperations().start(
        process.env.CCS_DIR as string,
        state.openProfile,
        'mac',
        () => new Promise<void>((resolve) => pendingOpens.push(resolve))
      );
  };

  try {
    for (const form of [
      ...fixtures.desktopProfileForms(),
      ...fixtures.desktopUsageForms(),
      ...fixtures.codexProfileForms(),
      ...fixtures.codexQuotaForms(),
      ...fixtures.antigravityInventoryForms(),
      ...fixtures.antigravityQuotaForms(),
    ]) {
      prepare(form);
      await run(
        form.name,
        form.route,
        form.rust,
        form.compared ?? true,
        form.role,
        form.reached ?? true
      );
    }
    for (const form of fixtures.hardenedForms()) {
      prepare({ setup: (dir) => form.setup(dir) });
      await run(`hardened ${form.name}`, form.route, form.rust);
    }

    const lease = (
      bridge as unknown as { lease: { connection: Parameters<typeof requestRust>[0] } | null }
    ).lease;
    const principal = { kind: 'session' as const, id: 'a'.repeat(64), role: 'admin' as const };
    const active = new Set<import('node:http').ClientRequest>();
    // Measured before the tables, so every mode samples the same worker state.
    let metrics: Report['metrics'];
    if (process.argv.includes('--measure')) {
      metrics = {};
      const sample = async (name: string, route: string, before: () => void) => {
        const samples: number[] = [];
        for (let index = 0; index < 45; index++) {
          before();
          const started = performance.now();
          const response = await fetch(`http://127.0.0.1:${port}${route}`, {
            headers: { 'x-session': '1' },
          });
          await response.arrayBuffer();
          if (index >= 5) samples.push(performance.now() - started);
        }
        samples.sort((a, b) => a - b);
        const child = fixtures.childRss();
        metrics![name] = {
          p50: Number(samples[Math.floor(samples.length * 0.5)].toFixed(3)),
          p95: Number(samples[Math.floor(samples.length * 0.95)].toFixed(3)),
          rssMiB: Number(((process.memoryUsage().rss + child) / 1024 / 1024).toFixed(1)),
          rustChildMiB: Number((child / 1024 / 1024).toFixed(1)),
        };
      };
      const forms = [
        ...fixtures.desktopProfileForms(),
        ...fixtures.desktopUsageForms(),
        ...fixtures.codexQuotaForms(),
        ...fixtures.antigravityInventoryForms(),
        ...fixtures.antigravityQuotaForms(),
      ];
      const use = (name: string) => {
        const form = forms.find((f) => f.name === name);
        assert.ok(form, name);
        prepare(form);
      };
      const uncached = () => {
        usageService.invalidateClaudeDesktopUsageCache();
        codexDashboard.invalidateCodexAuthProfilesCache();
      };
      use('three profiles, Unicode launchers');
      await sample('claude desktop-profiles', '/api/claude/desktop-profiles', () => {});
      use('usage: cached, signed out, empty, failed, unsupported, unconfigured');
      await sample(
        'claude usage (cache cleared per request)',
        '/api/claude/desktop-profiles/usage?platform=mac',
        uncached
      );
      use('quotas: available, stale cached, passed resets, expired, unavailable, retrieving');
      await sample('codex profiles (cache cleared per request)', '/api/codex/profiles', uncached);
      await sample(
        'codex quotas (cache cleared per request)',
        '/api/codex/profiles/quotas',
        uncached
      );
      use('inventory: paused update, runtime problem, Unicode, offsets');
      await sample('antigravity profiles', '/api/antigravity/profiles', () => {});
      use('ag quotas: every status, cache reason, plan and window form');
      await sample('antigravity quotas', '/api/antigravity/profiles/quotas', () => {});
      // Composition alone: the TS composer in process (off) or the Rust socket round trip
      // including input serialization and reply parsing (on).
      const accounts = fixtures.antigravityAccounts();
      const timed = async (name: string, once: () => Promise<unknown> | unknown) => {
        const samples: number[] = [];
        for (let index = 0; index < 45; index++) {
          const started = performance.now();
          await once();
          if (index >= 5) samples.push(performance.now() - started);
        }
        samples.sort((a, b) => a - b);
        metrics![name] = {
          p50: Number(samples[Math.floor(samples.length * 0.5)].toFixed(3)),
          p95: Number(samples[Math.floor(samples.length * 0.95)].toFixed(3)),
          rssMiB: Number(
            ((process.memoryUsage().rss + fixtures.childRss()) / 1024 / 1024).toFixed(1)
          ),
          rustChildMiB: Number((fixtures.childRss() / 1024 / 1024).toFixed(1)),
        };
      };
      if (mode === 'off')
        await timed('compose only: antigravity quotas', () =>
          publicReads.publicQuotas(accounts as never, NOW)
        );
      else if (lease)
        await timed('compose only: antigravity quotas', () =>
          requestRust(
            lease.connection,
            '/api/antigravity/profiles/quotas',
            principal,
            active,
            profileInput.rustAntigravityQuotasInput(accounts, NOW)
          )
        );
    }
    // Composition tables over the private socket: TS composer vs Rust, byte for byte.
    const tables: Report['tables'] = {};
    if (lease) {
      fixtures.resetCcs(ccs);
      const composers = {
        publicInventory: publicReads.publicInventory,
        publicQuotas: publicReads.publicQuotas,
        composeClaudeDesktopUsage: usageService.composeClaudeDesktopUsage,
        composeCodexAuthProfilesSummary: codexDashboard.composeCodexAuthProfilesSummary,
        composeCodexProfileQuotas: quotaService.composeCodexProfileQuotas,
        listClaudeDesktopProfileMetadata: profileService.listClaudeDesktopProfileMetadata,
        input: profileInput,
      };
      for (const entry of compositionTables(composers as never, ccs)) {
        const table = (tables[entry.table] ??= {
          total: 0,
          served: 0,
          tsErrors: 0,
          deferred: 0,
          refused: [],
          unexpected: [],
        });
        table.total++;
        entry.setup?.();
        let expected: string | undefined;
        try {
          expected = JSON.stringify(await entry.ts());
        } catch {
          expected = undefined;
        }
        const input = entry.input ? entry.input() : undefined;
        const reply =
          entry.input && input === null
            ? null
            : await requestRust(lease.connection, entry.route, principal, active, input);
        const label = `${entry.table}: ${entry.label}`;
        if (reply?.status === 200) {
          if (expected === undefined)
            table.unexpected.push(`${label}: Rust answered where TS threw`);
          else if (JSON.stringify(reply.body) !== expected)
            table.unexpected.push(`${label}: Rust differs from TS`);
          else table.served++;
        } else {
          table.refused.push(label);
          if (expected === undefined) table.tsErrors++;
          else if (entry.mayRefuse) table.deferred++;
          else table.unexpected.push(`${label}: refused an input TS answers`);
        }
      }
    }

    fs.writeFileSync(
      destination,
      JSON.stringify({
        mode,
        rows,
        cases,
        tables,
        ...(metrics ? { metrics } : {}),
      } satisfies Report)
    );
  } finally {
    for (const resolve of pendingOpens) resolve();
    server.close();
    bridge.stop();
    fs.rmSync(home, { recursive: true, force: true });
  }
  process.exit(0);
}
