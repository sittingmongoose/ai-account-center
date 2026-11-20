/**
 * R5/A2 owned Analytics parity: GET /api/accounts/analytics through the real TS router with the
 * analytics group on and the store hosted by the Rust service, against the same request's pure TS
 * computation (the service called without a composer). Every range preset, filter and zone here
 * must be byte-equal and served from Rust with no TS composition; a case Rust cannot prove falls
 * back to the TS path and stays equal. Fixture roots only; no provider call, no live scan.
 *
 *   bun tests/spec/analytics-owned-parity.ts   (needs dist/native/aac-http and the aac-store .node)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const DAY = 86_400_000;
const HOUR = 3_600_000;
const NOW = Date.parse('2026-11-14T21:10:00.000Z');

const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-r5-home-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_RUST_STORE: 'on',
  AAC_STORE_SHARE_BACKUP_DIR: 'off',
  AAC_STORE_MODULE: path.join(
    repo,
    'dist/native',
    `aac-store.${process.platform}-${process.arch}.node`
  ),
  AAC_RUST_GROUP_ANALYTICS: 'off',
});
const canonicalCcs = fs.realpathSync(ccs);
// Dynamic imports are the module-loading boundary this spec exercises: the fixture env and the
// os.homedir mock must be in place before the service modules read them at import time.
const { spyOn, mock } = await import('bun:test');
// Fixture homes only: a reader that ignores CCS_HOME must never reach a real private folder.
const fixtureOs = { ...os, homedir: () => home };
for (const name of ['os', 'node:os'])
  mock.module(name, () => ({ ...fixtureOs, default: fixtureOs }));

const express = (await import('express')).default;
const { RustService } = await import('../../src/web-server/rust-service');
const { createAccountAnalyticsRouter } =
  await import('../../src/web-server/routes/account-analytics-routes');
const { AccountAnalyticsService } =
  await import('../../src/web-server/services/account-analytics-service');
const { AccountAnalyticsActivityService } =
  await import('../../src/web-server/services/account-analytics-activity');
const composeModule = await import('../../src/web-server/services/account-analytics-compose');
const { loadAnalyticsRemoteCachedSources } =
  await import('../../src/web-server/services/analytics-remote-sources');
const { openAacStore, resetAacStoreForTests, closeAacStore } =
  await import('../../src/store/store-service');
const { storeObservationFromAccount } = await import('../../src/store/usage-observations');
const { buildActivityReports } = await import('../../src/store/activity-reports');
const { analyticsSessionKey } = await import('../../src/web-server/usage/analytics-session-key');
const fixtures = await import('./account-read-fixtures');
const { defaultAnalyticsProviderTable } =
  await import('../../src/web-server/services/account-analytics-providers');

const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(repo, 'dist/native/aac-http');
assert.ok(fs.existsSync(binary), 'owned parity needs a built aac-http binary');
const bridge = new RustService('on');
await bridge.start(binary);
assert.equal(bridge.stats.fallbacks, 0, 'the Rust service did not start');
resetAacStoreForTests();
const opened = await openAacStore();
assert.equal(opened.hosted, true, 'the store opened hosted');
const store = opened.store;
assert.ok(store, 'a hosted store');

// ---------------------------------------------------------------------------
// Seed the store: quota observations across the retained window, activity rows
// and sessions across 40 days, and one flagged (quality) codex read.
// ---------------------------------------------------------------------------
const accounts = fixtures.dashboardAccounts();
for (const at of [
  NOW - 25 * DAY,
  NOW - 12 * DAY,
  NOW - 4 * DAY,
  NOW - 26 * HOUR,
  NOW - HOUR,
  NOW - 5 * 60_000,
]) {
  const sampled = accounts.map((account, index) => ({
    ...account,
    sampledAt: new Date(at).toISOString(),
    fetchedAt: new Date(at).toISOString(),
    windows: account.windows.map((w) => ({
      ...w,
      usedPercent: typeof w.usedPercent === 'number' ? w.usedPercent + index : w.usedPercent,
    })),
  }));
  const observations = sampled
    .map((account) => storeObservationFromAccount(account, at, Number.POSITIVE_INFINITY))
    .filter((observation) => observation !== null);
  await store.record(observations);
}

function hourLabel(ms: number): string {
  return new Date(ms).toISOString().slice(0, 13).replace('T', ' ') + ':00';
}
function breakdown(model: string, provider: string, tokens: number, cost: number, events: number) {
  return {
    modelName: model,
    provider,
    inputTokens: tokens,
    outputTokens: Math.floor(tokens / 4),
    cacheCreationTokens: Math.floor(tokens / 8),
    cacheReadTokens: Math.floor(tokens / 2),
    cost,
    requestCount: events,
  };
}
function workerResult(tool: string, host: string, model: string, provider: string) {
  const hourly = [];
  const session = [];
  for (let d = 0; d < 40; d++) {
    const at = NOW - d * DAY - 3 * HOUR;
    const tokens = 1000 * (d + 1);
    const cost = 0.01 * (d + 1);
    const row = {
      hour: hourLabel(at),
      source: `${tool}-fixture`,
      inputTokens: tokens,
      outputTokens: Math.floor(tokens / 4),
      cacheCreationTokens: Math.floor(tokens / 8),
      cacheReadTokens: Math.floor(tokens / 2),
      cost,
      totalCost: cost,
      modelsUsed: [model],
      modelBreakdowns: [breakdown(model, provider, tokens, cost, d + 1)],
      requestCount: d + 1,
    };
    hourly.push(row);
    if (d % 5 === 0) {
      const sessionId = analyticsSessionKey(tool as never, `${tool}-session-${d}`);
      session.push({
        sessionId,
        projectPath: '',
        source: `${tool}-fixture`,
        inputTokens: tokens,
        outputTokens: Math.floor(tokens / 4),
        cacheCreationTokens: Math.floor(tokens / 8),
        cacheReadTokens: Math.floor(tokens / 2),
        cost,
        totalCost: cost,
        firstActivity: new Date(at - 2 * HOUR).toISOString(),
        lastActivity: new Date(at).toISOString(),
        versions: [],
        modelsUsed: [model],
        modelBreakdowns: [breakdown(model, provider, tokens, cost, d + 1)],
        target: tool === 'omp' ? 'omp' : tool,
      });
    }
  }
  return { tool, host, hourly, session, daily: [], monthly: [], eventCount: 0 };
}
const states = [
  {
    tool: 'claude',
    host: 'mac',
    state: 'ok',
    lastScanAt: new Date(NOW - HOUR).toISOString(),
    rowCount: 0,
    detail: null,
  },
  {
    tool: 'codex',
    host: 'mac',
    state: 'ok',
    lastScanAt: new Date(NOW - HOUR).toISOString(),
    rowCount: 0,
    detail: null,
  },
  {
    tool: 'omp',
    host: 'ubuntu',
    state: 'ok',
    lastScanAt: new Date(NOW - HOUR).toISOString(),
    rowCount: 0,
    detail: null,
  },
] as never[];
const reports = buildActivityReports({
  local: new Map(),
  remote: [
    {
      tool: 'claude',
      host: 'mac',
      data: workerResult('claude', 'mac', 'claude-sonnet-4-5', 'anthropic'),
    },
    { tool: 'codex', host: 'mac', data: workerResult('codex', 'mac', 'gpt-5.1-codex', 'openai') },
    { tool: 'omp', host: 'ubuntu', data: workerResult('omp', 'ubuntu', 'kimi-k3', 'kimi-code') },
  ] as never[],
  states,
  windowFromMs: NOW - 40 * DAY,
  now: NOW,
  localComplete: () => true,
});
// One flagged read: the retired Codex cache's doubled output counts (quality bit 1).
reports.push({
  schema: 'aac.usage-report.v1',
  computer: 'mac',
  tool: 'codex',
  readAt: new Date(NOW - 2 * HOUR).toISOString(),
  state: 'ok',
  complete: true,
  coverFrom: new Date(Math.floor((NOW - 40 * DAY) / HOUR) * HOUR).toISOString(),
  cause: null,
  cells: [
    {
      hour: new Date(Math.floor((NOW - 2 * DAY - 5 * HOUR) / HOUR) * HOUR).toISOString(),
      sessionKey: null,
      route: 'openai',
      modelRaw: 'gpt-5.1-codex',
      inputTokens: 500,
      outputTokens: 900,
      cacheCreationTokens: 0,
      cacheReadTokens: 100,
      costUsd: 0.02,
      fallbackCostUsd: 0,
      events: 3,
      quality: ['outputIncludesReasoningTwice'],
    },
  ],
  sessions: [],
} as never);
const ingest = await store.activityIngest(reports);
assert.equal(
  ingest.refused.length,
  0,
  `activity ingest refused: ${JSON.stringify(ingest.refused)}`
);

// ---------------------------------------------------------------------------
// Services and router
// ---------------------------------------------------------------------------
const cacheDir = path.join(canonicalCcs, 'cache');
await fixtures.prepareRemoteCaches(cacheDir);

interface Services {
  activity: InstanceType<typeof AccountAnalyticsActivityService>;
  analytics: InstanceType<typeof AccountAnalyticsService>;
}

// The fixture store is read-only for the activity services: their collection passes update the
// in-memory grid, but no ingest lands between the reference read and the owned read, so both
// answer the same stored state (the parity of the reads, not of the collector's timing).
const frozenStore = new Proxy(store, {
  get(target, prop, receiver) {
    if (prop === 'activityIngest') return async () => ({ stored: 0, duplicates: 0, refused: [] });
    const value = Reflect.get(target, prop, receiver);
    return typeof value === 'function' ? value.bind(target) : value;
  },
});

function makeServices(dashboardAccountsOverride = accounts): Services {
  const activity = new AccountAnalyticsActivityService({
    scope: () => canonicalCcs,
    now: () => NOW,
    requests: () => [],
    remote: async (cutoff: number) => loadAnalyticsRemoteCachedSources(cutoff, { cacheDir }),
    remoteCached: (cutoff: number) => loadAnalyticsRemoteCachedSources(cutoff, { cacheDir }),
    localScannerOwned: () => [],
    refreshIntervalSeconds: () => 60,
    historyStore: async () => frozenStore,
  });
  const analytics = new AccountAnalyticsService({
    getDashboard: async () => fixtures.dashboard(dashboardAccountsOverride) as never,
    getActivity: (query, from, to, options) => activity.get(query, from, to, options),
    getActivityLive: (query, from, to) => activity.liveForOwned(query, from, to),
    scope: () => canonicalCcs,
    now: () => NOW,
    samplingIntervalMs: () => 60_000,
    freshBudgetMs: 10_000,
  });
  return { activity, analytics };
}

// Settle one activity service so its published grid is deterministic.
async function settle(activity: InstanceType<typeof AccountAnalyticsActivityService>) {
  const probe = {
    platform: 'mac',
    range: '7d',
    provider: 'all',
    account: 'all',
    tz: 'UTC',
  } as never;
  for (let attempt = 0; attempt < 400; attempt++) {
    const answer = await activity.get(probe, NOW - 7 * DAY, NOW, { tz: 'UTC' });
    if (!answer.refreshing && answer.status !== 'loading') return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error('the fixture activity service did not settle');
}

const legacy = { count: 0 };
const originalCompose = composeModule.composeAccountAnalytics;
spyOn(composeModule, 'composeAccountAnalytics').mockImplementation(((...args: unknown[]) => {
  legacy.count++;
  return (originalCompose as (...a: unknown[]) => unknown)(...args);
}) as never);

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
let current: Services | null = null;
app.use(
  '/api/accounts',
  createAccountAnalyticsRouter({
    getAnalytics: (query, composer, owned) =>
      (current as Services).analytics.get(query, composer, owned),
    providerIds: () => defaultAnalyticsProviderTable().map((entry) => entry.id),
  })
);
const server = app.listen(0, '127.0.0.1');
const listening = Promise.withResolvers<void>();
server.once('listening', listening.resolve);
await listening.promise;
const port = (server.address() as { port: number }).port;

interface Case {
  name: string;
  query: string;
  /** Rust must serve this case from the owned path (no decline). */
  owned?: boolean;
  accounts?: typeof accounts;
}
const cases: Case[] = [
  { name: '24h UTC', query: 'range=24h&tz=UTC', owned: true },
  { name: '7d UTC', query: 'range=7d&tz=UTC', owned: true },
  { name: '30d UTC', query: 'range=30d&tz=UTC', owned: true },
  { name: '90d UTC (daily buckets)', query: 'range=90d&tz=UTC', owned: true },
  { name: 'all UTC', query: 'range=all&tz=UTC', owned: true },
  { name: 'month UTC', query: 'range=month&tz=UTC', owned: true },
  {
    name: 'custom days UTC',
    query: 'range=custom&from=2026-11-10&to=2026-11-13&tz=UTC',
    owned: true,
  },
  { name: '7d New York', query: 'range=7d&tz=America/New_York', owned: true },
  { name: '30d Tokyo', query: 'range=30d&tz=Asia/Tokyo', owned: true },
  { name: '90d New York (daily)', query: 'range=90d&tz=America/New_York', owned: true },
  { name: 'all Tokyo', query: 'range=all&tz=Asia/Tokyo', owned: true },
  { name: 'month London', query: 'range=month&tz=Europe/London', owned: true },
  {
    name: 'custom days Tokyo',
    query: 'range=custom&from=2026-10-20&to=2026-11-13&tz=Asia/Tokyo',
    owned: true,
  },
  { name: 'provider codex', query: 'range=7d&provider=codex&tz=UTC', owned: true },
  { name: 'provider claude', query: 'range=30d&provider=claude&tz=UTC', owned: true },
  { name: 'provider other', query: 'range=7d&provider=other&tz=UTC', owned: true },
  {
    name: 'provider kimi-code (omp rows)',
    query: 'range=7d&provider=kimi-code&tz=UTC',
    owned: true,
  },
  { name: 'one account (guarded)', query: 'range=7d&account=codex:work&tz=UTC', owned: true },
  {
    name: 'one account, provider filter (guarded)',
    query: 'range=30d&account=codex:work&provider=codex&tz=UTC',
    owned: true,
  },
  { name: 'refresh true', query: 'range=7d&refresh=true&tz=UTC', owned: true },
  {
    name: 'refresh true, one account',
    query: 'range=7d&refresh=true&account=codex:work&tz=UTC',
    owned: true,
  },
  { name: 'windows platform', query: 'range=7d&platform=windows&tz=UTC', owned: true },
  {
    name: 'non-ASCII address declines',
    query: 'range=7d&tz=UTC',
    owned: false,
    accounts: [
      {
        ...accounts[0],
        id: 'codex:work',
        email: 'Jörg@Example.com',
      },
    ] as typeof accounts,
  },
];

const run = async (query: string): Promise<string> => {
  const response = await fetch(`http://127.0.0.1:${port}/api/accounts/analytics?${query}`, {
    headers: { 'x-session': '1' },
  });
  assert.equal(response.status, 200, `${query}: status ${response.status}`);
  return response.text();
};

let served = 0;
let declined = 0;
/** The first differing leaf of two answers, for a failure message that names the field. */
function firstDiff(a: unknown, b: unknown, at = ''): string | null {
  if (Object.is(a, b)) return null;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return `${at}.length ${a.length} != ${b.length}`;
    for (let i = 0; i < a.length; i++) {
      const diff = firstDiff(a[i], b[i], `${at}[${i}]`);
      if (diff) return diff;
    }
    return null;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a as object);
    const kb = Object.keys(b as object);
    if (ka.join(',') !== kb.join(',')) return `${at} keys ${ka.join(',')} != ${kb.join(',')}`;
    for (const key of ka) {
      const diff = firstDiff(
        (a as Record<string, unknown>)[key],
        (b as Record<string, unknown>)[key],
        `${at}.${key}`
      );
      if (diff) return diff;
    }
    return null;
  }
  return `${at}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`;
}

try {
  for (const item of cases) {
    // The pure TS computation: the same service without a composer or owned read.
    const referenceServices = makeServices(item.accounts ?? accounts);
    await settle(referenceServices.activity);
    const request = Object.fromEntries(new URLSearchParams(item.query));
    const reference = await referenceServices.analytics.get({
      platform: 'mac',
      range: '7d',
      provider: 'all',
      account: 'all',
      ...request,
      ...(request.refresh !== undefined ? { refresh: request.refresh === 'true' } : {}),
    } as never);
    const referenceBody = JSON.stringify(reference);

    // The owned read through the guarded route.
    current = makeServices(item.accounts ?? accounts);
    await settle(current.activity);
    const before = { ...bridge.stats };
    const calls = legacy.count;
    process.env.AAC_RUST_GROUP_ANALYTICS = 'on';
    const body = await run(item.query);
    process.env.AAC_RUST_GROUP_ANALYTICS = 'off';
    const servedDelta = bridge.stats.served - before.served;
    const fallbackDelta = bridge.stats.fallbacks - before.fallbacks;
    assert.equal(
      body,
      referenceBody,
      `${item.name}: owned answer differs: ${firstDiff(JSON.parse(body), JSON.parse(referenceBody))}`
    );
    if (item.owned) {
      assert.equal(servedDelta, 1, `${item.name}: the owned path did not serve`);
      assert.equal(legacy.count - calls, 0, `${item.name}: the TS composer ran`);
      served++;
    } else {
      assert.ok(fallbackDelta >= 1, `${item.name}: the decline was not logged`);
      declined++;
    }
    process.stdout.write(`ok ${item.name}${item.owned ? ' (owned)' : ' (declined)'}\n`);
  }
  process.stdout.write(
    `analytics-owned: ${cases.length} cases, ${served} owned-served, ${declined} declined-equal\n`
  );
} finally {
  server.close();
  await closeAacStore().catch(() => undefined);
  bridge.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
