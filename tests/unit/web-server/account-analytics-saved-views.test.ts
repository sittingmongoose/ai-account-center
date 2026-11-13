import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { AccountAnalyticsService } from '../../../src/web-server/services/account-analytics-service';
import {
  AnalyticsSavedViews,
  MAX_SAVED_VIEWS,
  validSavedViews,
} from '../../../src/web-server/services/account-analytics-saved-views';
import type { AccountAnalyticsHistoryStore } from '../../../src/web-server/services/account-analytics-history';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import type {
  AccountAnalytics,
  AccountAnalyticsActivity,
  AccountAnalyticsQuery,
} from '../../../src/web-server/services/account-analytics-types';

// The last answer of each selection is kept and served at once while the next one is computed (synthetic data only).
let sandbox = '';
let previousCcsHome: string | undefined;
beforeAll(() => {
  previousCcsHome = process.env.CCS_HOME;
  sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-analytics-views-'));
  process.env.CCS_HOME = sandbox;
});
afterAll(() => {
  if (previousCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousCcsHome;
  fs.rmSync(sandbox, { recursive: true, force: true });
});

const NOW = Date.parse('2026-11-13T10:00:00Z');
const QUERY: AccountAnalyticsQuery = {
  platform: 'mac',
  range: '7d',
  provider: 'all',
  account: 'all',
};

function activity(events: number, refreshing = false): AccountAnalyticsActivity {
  return {
    status: refreshing ? 'cached' : 'ok',
    refreshing,
    scope: 'multi-host-cli',
    timezone: 'UTC',
    accountAttribution: 'unavailable',
    costBasis: 'estimated-api-equivalent',
    fetchedAt: new Date(NOW).toISOString(),
    message: 'fixture',
    totals: {
      inputTokens: events,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      estimatedCostUsd: 0,
      usageEvents: events,
    },
    providers: [],
    byDay: [],
    byHour: [],
    models: [],
  } as unknown as AccountAnalyticsActivity;
}

const board: AccountDashboard = {
  schemaVersion: 1,
  updatedAt: new Date(NOW).toISOString(),
  accounts: [],
  settings: { refreshIntervalSeconds: 60 },
} as unknown as AccountDashboard;

function memory(): AccountAnalyticsHistoryStore {
  return {
    async read() {
      return null;
    },
    async write() {},
  };
}

/** A collection whose answers the test releases one by one. */
function gated() {
  let events = 0;
  let release: (() => void) | null = null;
  let gate: Promise<void> = Promise.resolve();
  return {
    hold() {
      gate = new Promise<void>((resolve) => {
        release = resolve;
      });
    },
    release() {
      release?.();
      release = null;
    },
    set(next: number) {
      events = next;
    },
    async getActivity() {
      await gate;
      return activity(events);
    },
  };
}

const events = (answer: AccountAnalytics) =>
  (answer.activity.totals as unknown as { usageEvents: number }).usageEvents;

describe('analytics saved views', () => {
  it('serves the newest saved answer at once, marked stale and refreshing, while the next is computed', async () => {
    let now = NOW;
    const logs = gated();
    const service = new AccountAnalyticsService({
      getDashboard: async () => board,
      getActivity: () => logs.getActivity(),
      createHistoryStore: memory,
      now: () => now,
      freshBudgetMs: 30,
    });
    logs.set(10);
    const first = await service.get(QUERY);
    expect(events(first)).toBe(10);
    expect(first.stale).toBe(false);
    expect(first.asOf).toBe(first.updatedAt);

    // the next computation is slow: the read is answered with the saved answer within the budget; five minutes on,
    // past the refresh interval, it is said to be older than the read
    now += 5 * 60_000;
    logs.set(25);
    logs.hold();
    const started = performance.now();
    const served = await service.get(QUERY);
    expect(performance.now() - started).toBeLessThan(1_000);
    expect(events(served)).toBe(10);
    expect(served.stale).toBe(true);
    expect(served.refreshing).toBe(true);
    expect(served.asOf).toBe(first.updatedAt);

    // a read meanwhile joins the same computation; once it lands the newer numbers are current
    const joined = service.get(QUERY);
    logs.release();
    const next = await joined;
    expect(events(next)).toBe(25);
    expect(next.stale).toBe(false);
    expect(Date.parse(next.asOf as string)).toBe(now);
  });

  it('a selection whose computation is slow is answered from the saved view at once; current while it is young', async () => {
    let now = NOW;
    let slow = 0;
    const service = new AccountAnalyticsService({
      getDashboard: async () => {
        // the dashboard read waits on its providers
        if (slow) await new Promise((resolve) => setTimeout(resolve, slow));
        return board;
      },
      getActivity: async () => activity(now === NOW ? 5 : 6),
      createHistoryStore: memory,
      now: () => now,
      freshBudgetMs: 40,
      samplingIntervalMs: () => 60_000,
    });
    await service.get(QUERY);
    slow = 300;
    now += 20_000;
    // the computation outlives the budget: the saved answer, 20 s old, is current
    let started = performance.now();
    const young = await service.get(QUERY);
    expect(performance.now() - started).toBeLessThan(250);
    expect(young.stale).toBe(false);
    expect(events(young)).toBe(5);
    await new Promise((resolve) => setTimeout(resolve, 400));
    // that selection is now known to be slow: the next read does not wait at all
    now += 2 * 60_000;
    started = performance.now();
    const old = await service.get(QUERY);
    expect(performance.now() - started).toBeLessThan(30);
    expect(old.stale).toBe(true);
    expect(old.refreshing).toBe(true);
    expect(events(old)).toBe(6);
  });

  it('answers within the budget with the fresh answer when the computation is quick', async () => {
    let now = NOW;
    const logs = gated();
    const service = new AccountAnalyticsService({
      getDashboard: async () => board,
      getActivity: () => logs.getActivity(),
      createHistoryStore: memory,
      now: () => now,
    });
    logs.set(3);
    await service.get(QUERY);
    now += 60_000;
    logs.set(4);
    const answer = await service.get(QUERY);
    expect(events(answer)).toBe(4);
    expect(answer.stale).toBe(false);
  });

  it('a first read with nothing saved waits for its answer; a failure with nothing saved fails', async () => {
    let fail = true;
    const service = new AccountAnalyticsService({
      getDashboard: async () => {
        if (fail) throw new Error('fixture dashboard down');
        return board;
      },
      getActivity: async () => activity(1),
      createHistoryStore: memory,
      now: () => now,
      freshBudgetMs: 1,
    });
    let now = NOW;
    await expect(service.get(QUERY)).rejects.toThrow('fixture dashboard down');
    fail = false;
    const answer = await service.get(QUERY);
    expect(answer.stale).toBe(false);
    expect(events(answer)).toBe(1);
    // a later failure keeps the saved answer on screen, said to be older than this read once it is
    fail = true;
    expect((await service.get(QUERY)).stale).toBe(false);
    now += 5 * 60_000;
    const kept = await service.get(QUERY);
    expect(kept.stale).toBe(true);
    expect(events(kept)).toBe(1);
  });

  it('a restarted server answers its first read from the saved file, then computes afresh', async () => {
    const scope = fs.mkdtempSync(path.join(sandbox, 'scope-'));
    let now = NOW;
    const logs = gated();
    const deps = {
      getDashboard: async () => board,
      getActivity: () => logs.getActivity(),
      createHistoryStore: memory,
      scope: () => scope,
      now: () => now,
      persistViews: true,
      freshBudgetMs: 30,
    };
    const before = new AccountAnalyticsService(deps);
    logs.set(42);
    const computed = await before.get(QUERY);
    before.flushSavedViews();
    const file = path.join(scope, 'account-analytics', 'analytics-views-v1.json');
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readdirSync(path.dirname(file)).filter((name) => name.endsWith('.tmp'))).toEqual([]);

    // the restart: a new service, whose first computation is slow (its logs are still being read)
    now += 5 * 60_000;
    logs.set(50);
    logs.hold();
    const after = new AccountAnalyticsService(deps);
    const first = await after.get(QUERY);
    expect(events(first)).toBe(42);
    expect(first.stale).toBe(true);
    expect(first.refreshing).toBe(true);
    expect(first.asOf).toBe(computed.updatedAt);
    logs.release();
    await new Promise((resolve) => setTimeout(resolve, 20));
    const next = await after.get(QUERY);
    expect(events(next)).toBe(50);
    expect(next.stale).toBe(false);
    after.flushSavedViews();
  });

  it('a changed selection never reuses another selection’s answer', async () => {
    const logs = gated();
    const service = new AccountAnalyticsService({
      getDashboard: async () => board,
      getActivity: () => logs.getActivity(),
      createHistoryStore: memory,
      now: () => NOW,
      freshBudgetMs: 1,
    });
    logs.set(7);
    await service.get(QUERY);
    logs.hold();
    // 30d has nothing saved: it waits for its own answer instead of being handed the 7d one
    const pending = service.get({ ...QUERY, range: '30d' });
    let settled = false;
    void pending.then(() => {
      settled = true;
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(settled).toBe(false);
    logs.release();
    expect((await pending).range.preset).toBe('30d');
  });
});

describe('the saved views file', () => {
  const answer = (updatedAt: string): AccountAnalytics =>
    ({
      schemaVersion: 1,
      updatedAt,
      range: { preset: '7d' },
      activity: activity(1),
      accounts: [],
      providers: [],
    }) as unknown as AccountAnalytics;

  it('keeps the newer answer when two computations finish out of order, and bounds the selections', () => {
    const views = new AnalyticsSavedViews('relative-scope', false);
    views.set('a', answer('2026-11-13T10:01:00Z'));
    views.set('a', answer('2026-11-13T10:00:00Z'));
    expect(views.get('a')?.updatedAt).toBe('2026-11-13T10:01:00Z');
    for (let i = 0; i < MAX_SAVED_VIEWS + 3; i++)
      views.set(`k${i}`, answer('2026-11-13T10:00:00Z'));
    expect(views.get('a')).toBeUndefined();
    expect(views.get(`k${MAX_SAVED_VIEWS + 2}`)).toBeDefined();
  });

  it('ignores another version, a malformed answer or an oversized list', () => {
    const good = { version: 1, views: [['k', answer('2026-11-13T10:00:00Z')]] };
    expect(validSavedViews(good)).toBe(true);
    expect(validSavedViews({ ...good, version: 2 })).toBe(false);
    expect(validSavedViews({ version: 1, views: [['k', { schemaVersion: 1 }]] })).toBe(false);
    expect(
      validSavedViews({
        version: 1,
        views: Array.from({ length: MAX_SAVED_VIEWS + 1 }, (_, i) => [
          `k${i}`,
          answer('2026-11-13T10:00:00Z'),
        ]),
      })
    ).toBe(false);
    const scope = fs.mkdtempSync(path.join(sandbox, 'bad-'));
    fs.mkdirSync(path.join(scope, 'account-analytics'));
    fs.writeFileSync(path.join(scope, 'account-analytics', 'analytics-views-v1.json'), '{not json');
    expect(new AnalyticsSavedViews(scope, true).get('k')).toBeUndefined();
  });
});
