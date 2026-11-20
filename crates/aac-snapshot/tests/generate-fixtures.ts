/**
 * Parity fixture generator for the Rust aac-snapshot crate (cut gap G18).
 *
 * Dumps the golden fixture's inputs and a seeded differential corpus, with the TypeScript
 * writer's exact output bytes for every case. The Rust integration test (tests/parity.rs)
 * rebuilds each case and must match byte for byte. Run from the repo root:
 *
 *   bun crates/aac-snapshot/tests/generate-fixtures.ts
 *
 * Pure fixtures: no provider calls, no real home or share reads, no clock.
 */
import fs from 'fs';
import path from 'path';
import {
  GOLDEN_NOW,
  GOLDEN_SIGN_IN_NOW,
  goldenEvent,
  goldenSignIn,
  goldenSignInRenewal,
} from '../../../tests/unit/web-server/agent-usage-golden/fixture';
import { buildSnapshotFiles } from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { renderSnapshotMarkdown } from '../../../src/web-server/agent-usage/agent-usage-markdown';
import { RecentReadings } from '../../../src/web-server/agent-usage/agent-usage-history';
import { accountAnalyticsIdentity } from '../../../src/web-server/services/account-analytics-history';
import { longBurnKey } from '../../../src/web-server/services/account-analytics-service';
import type { AccountAnalyticsSampleEvent } from '../../../src/web-server/services/account-analytics-service';
import type { AccountAnalyticsHistoryData, AccountAnalyticsObservation } from '../../../src/web-server/services/account-analytics-history';
import type { DashboardAccount, DashboardAccountWindow, DashboardAccountStatus, UsageCacheReason } from '../../../src/web-server/services/account-dashboard-types';

/** One sample event as the fixtures store it: the long-burn Map flattened to entries. */
interface SerializedEvent {
  accounts: DashboardAccount[];
  history: AccountAnalyticsHistoryData | null;
  sampledAt: number;
  longBurn: Array<[string, unknown]> | null;
}

const OUT = path.join(__dirname, 'fixtures');
fs.mkdirSync(OUT, { recursive: true });

const MINUTE = 60_000;
const iso = (ms: number) => new Date(ms).toISOString();

/** The golden inputs, exactly as the golden test drives the TS writer. */
const golden = {
  refreshSeconds: 60,
  signInNow: GOLDEN_SIGN_IN_NOW,
  recentSamples: [] as Array<{ accounts: DashboardAccount[]; sampledAt: number }>,
  event: serializeEvent(goldenEvent()),
  signIn: goldenSignIn(),
  signInRenewal: goldenSignInRenewal(),
};
fs.writeFileSync(path.join(OUT, 'golden-input.json'), JSON.stringify(golden, null, 1));

function serializeEvent(event: AccountAnalyticsSampleEvent): SerializedEvent {
  return {
    accounts: event.accounts,
    history: event.history,
    sampledAt: event.sampledAt,
    longBurn: event.longBurn ? [...event.longBurn.entries()] : null,
  };
}

/** mulberry32: the corpus is reproducible from its seed. */
function rng(seed: number) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const PROVIDERS = ['codex', 'claude', 'antigravity', 'muse', 'cursor', 'kimi-code', 'qwen', 'zai', 'opencode-go'];
const PROVIDER_LABELS: Record<string, string> = {
  codex: 'Codex', claude: 'Claude', antigravity: 'Antigravity', muse: 'Muse Code', cursor: 'Cursor',
  'kimi-code': 'Kimi Code', qwen: 'Qwen', zai: 'Z.ai', 'opencode-go': 'OpenCode Go',
};
const PLATFORMS = ['ubuntu', 'mac', 'windows'];
const STATUSES = ['ok', 'ok', 'ok', 'cached', 'unavailable', 'error', 'needs_sign_in'];
const CACHE_REASONS: Array<UsageCacheReason | undefined> = ['read_failed', 'rate_limited', 'refresh_pending', 'source_changed', undefined];
const SOURCES = [
  'Codex usage on Ubuntu', 'Verified quota on Ubuntu (fallback)', 'Z.ai usage on Mac',
  'Claude quota on Windows', 'Muse Code usage', '/home/dev/.claude/secret-path', 'Kimi Code usage on Nas1',
];
const PLANS = ['Pro', 'Max', 'Ultra', 'Coding Plan', null, 'Free', 'Team'];
const KEYS = ['five_hour', 'weekly', 'seven_day', 'prepaid', 'weekly_spend', 'session_reset_allowance'];
const UNITS = [null, 'USD', 'credits', 'resets'];
const SIGNIN_STATES = ['ok', 'warn', 'critical', 'logged_out', 'unavailable', 'unsupported'];

function pick<T>(rand: () => number, items: readonly T[]): T {
  return items[Math.floor(rand() * items.length)];
}

function chance(rand: () => number, p: number): boolean {
  return rand() < p;
}

function percent(rand: () => number): number | null {
  if (chance(rand, 0.2)) return null;
  const value = rand() * 100;
  return chance(rand, 0.5) ? Math.round(value) : Math.round(value * 10) / 10;
}

function makeWindow(rand: () => number, now: number, index: number): DashboardAccountWindow {
  const kind = pick(rand, ['rate_limit', 'rate_limit', 'rate_limit', 'balance', 'spend', 'extra_usage'] as const);
  const usedPercent = kind === 'rate_limit' ? percent(rand) : chance(rand, 0.7) ? null : percent(rand);
  const resetAt = chance(rand, 0.75) ? iso(now + Math.floor((rand() - 0.35) * 30 * 60) * MINUTE) : null;
  const cached = chance(rand, 0.15);
  const window: DashboardAccountWindow = {
    key: KEYS[index % KEYS.length] + (chance(rand, 0.2) ? `_grant_${index}` : ''),
    label: `Window ${index}`,
    usedPercent,
    remainingPercent: usedPercent === null ? null : Math.round((100 - usedPercent) * 10) / 10,
    resetAt,
    windowMinutes: chance(rand, 0.8) ? pick(rand, [300, 10080, 60]) : null,
    used: chance(rand, 0.4) ? Math.round(rand() * 500) / 10 : null,
    limit: null,
    unit: pick(rand, UNITS),
  };
  if (window.used !== null && chance(rand, 0.7)) window.limit = window.used + Math.round(rand() * 200);
  if (window.limit !== null && chance(rand, 0.8)) window.remaining = Math.round((window.limit - window.used!) * 10) / 10;
  if (chance(rand, 0.2)) window.expiresAt = iso(now + Math.floor(rand() * 40) * 24 * 60 * MINUTE);
  if (chance(rand, 0.15)) window.clears = chance(rand, 0.8) ? ['five_hour', 'seven_day'] : ['Bad Slug', 'five_hour'];
  if (chance(rand, 0.1)) window.unlimited = true;
  if (chance(rand, 0.2)) {
    window.poolId = `pool-${pick(rand, ['a', 'b'])}`;
    window.poolLabel = `Pool ${window.poolId.slice(-1).toUpperCase()}`;
  }
  if (chance(rand, 0.2)) window.modelIds = ['model-one', 'model_two', 'bad model!'];
  if (cached) {
    window.status = 'cached';
    window.sampledAt = iso(now - Math.floor(rand() * 240) * MINUTE);
  }
  return window;
}

function makeAccount(rand: () => number, now: number, index: number): DashboardAccount {
  const provider = pick(rand, PROVIDERS);
  const status = pick(rand, STATUSES);
  const sampledAt = iso(now - Math.floor(rand() * 200) * MINUTE);
  const account: DashboardAccount = {
    id: `${provider}:account-${index}`,
    provider: provider as DashboardAccount['provider'],
    providerLabel: PROVIDER_LABELS[provider],
    label: `Account ${index}`,
    email: chance(rand, 0.8) ? `user${index}@example.com` : null,
    plan: pick(rand, PLANS),
    platform: pick(rand, PLATFORMS) as DashboardAccount['platform'],
    source: pick(rand, SOURCES),
    status: status as DashboardAccount['status'],
    message: 'private collector message never published',
    fetchedAt: sampledAt,
    sampledAt,
    isActive: chance(rand, 0.5),
    windows: status === 'needs_sign_in' && chance(rand, 0.5) ? [] : Array.from({ length: Math.floor(rand() * 4) }, (_, w) => makeWindow(rand, now, w)),
    capabilities: {
      codexProfile: provider === 'codex' && chance(rand, 0.7) ? `profile-${index}` : null,
      claudeProfileId: provider === 'claude' && chance(rand, 0.7) ? `claude-${index}` : null,
      claudePlatforms: [],
    },
  };
  if (chance(rand, 0.15)) account.hidden = true;
  if (chance(rand, 0.15)) account.host = pick(rand, ['nas1', 'alexis', 'mac', 'windows']);
  if (provider === 'claude' && chance(rand, 0.4)) account.planClass = pick(rand, ['max', 'pro'] as const);
  if (status === 'cached') {
    const reason = pick(rand, CACHE_REASONS);
    if (reason) account.cacheReason = reason;
  }
  return account;
}

function makeHistory(rand: () => number, accounts: DashboardAccount[], now: number): AccountAnalyticsHistoryData {
  const records: AccountAnalyticsObservation[] = [];
  for (const account of accounts) {
    if (!chance(rand, 0.7) || account.windows.length === 0) continue;
    const count = 1 + Math.floor(rand() * 7);
    for (let r = 0; r < count; r++) {
      const at = now - Math.floor(rand() * 7 * 24 * 60) * MINUTE;
      const window = { ...account.windows[0] };
      window.usedPercent = percent(rand);
      window.remainingPercent = window.usedPercent === null ? null : 100 - window.usedPercent;
      if (chance(rand, 0.15)) {
        // A reset-only reading: marks a period boundary, never a rate.
        window.usedPercent = null;
        window.remainingPercent = null;
        window.resetAt = iso(at + 5 * MINUTE);
      }
      if (chance(rand, 0.1)) window.unit = null; // a dropped unit still bounds the period
      if (window.status === 'cached') {
        window.sampledAt = iso(at - 30 * MINUTE);
      }
      records.push({
        identity: accountAnalyticsIdentity(account),
        accountId: account.id,
        sampledAt: iso(at),
        observedAt: iso(at),
        platform: account.platform,
        source: account.source,
        status: 'ok' as DashboardAccountStatus,
        isActive: true,
        windows: [window],
      });
    }
  }
  records.sort((a, b) => Date.parse(a.sampledAt) - Date.parse(b.sampledAt));
  return { schemaVersion: 1 as const, collectedSince: iso(now - 30 * 24 * 60 * MINUTE), records };
}

function makeLongBurn(rand: () => number, accounts: DashboardAccount[]) {
  const entries: Array<[string, unknown]> = [];
  for (const account of accounts) {
    for (const window of account.windows) {
      if (typeof window.usedPercent !== 'number' || (window.kind ?? 'rate_limit') !== 'rate_limit') continue;
      if (!chance(rand, 0.5)) continue;
      const span = () => ({
        spanMs: 0,
        consumed: chance(rand, 0.8) ? Math.round(rand() * 5000) : null,
        rate: chance(rand, 0.8) ? Math.round(rand() * 2000) / 100 : null,
        coverage: Math.round(rand() * 100) / 100,
      });
      const last7Days = { ...span(), spanMs: 7 * 24 * 3600_000 };
      const last30Days = { ...span(), spanMs: 30 * 24 * 3600_000 };
      entries.push([longBurnKey(account, window), { last7Days, last30Days }]);
    }
  }
  return entries.length ? entries : null;
}

function makeSignIn(rand: () => number, now: number) {
  const count = Math.floor(rand() * 6);
  const rows = [];
  for (let i = 0; i < count; i++) {
    const state = pick(rand, SIGNIN_STATES);
    const row: Record<string, unknown> = {
      name: `profile-${i}`,
      provider: chance(rand, 0.8) ? 'claude' : 'muse',
      platform: pick(rand, ['ubuntu', 'darwin', 'win32']),
      machine: pick(rand, ['vm', 'mac', 'windows', 'nas1']),
      state,
      fixCommand: chance(rand, 0.7) ? `claude auth login --profile p${i}` : '',
    };
    if (chance(rand, 0.7)) row.accessExpiresAt = iso(now + Math.floor((rand() - 0.3) * 48 * 60) * MINUTE);
    if (chance(rand, 0.7)) row.refreshExpiresAt = iso(now + Math.floor((rand() - 0.2) * 40 * 24 * 60) * MINUTE);
    if (chance(rand, 0.6)) row.loggedOutAt = iso(now + Math.floor((rand() - 0.3) * 40 * 24 * 60) * MINUTE);
    if (state === 'unavailable' && chance(rand, 0.7)) row.error = 'ssh: connect refused (timed out)';
    if (chance(rand, 0.3)) {
      row.autoRenew = true;
      row.email = `renew${i}@example.com`;
      if (chance(rand, 0.7)) row.renewFrom = iso(now + Math.floor(rand() * 30 * 24 * 60) * MINUTE);
      if (chance(rand, 0.7)) {
        row.lastResult = pick(rand, ['renewed', 'waiting_for_you', 'login_failed']);
        row.lastRenewAt = iso(now - Math.floor(rand() * 24 * 60) * MINUTE);
      }
    }
    rows.push(row);
  }
  return rows;
}

interface Case {
  seed: number;
  refreshSeconds: number;
  signInNow: number;
  recentSamples: Array<{ accounts: DashboardAccount[]; sampledAt: number }>;
  event: SerializedEvent;
  signIn: unknown[];
  signInRenewal: unknown | null;
  expected: { json: string; markdown: string; history: string };
}

const cases: Case[] = [];
for (let seed = 1; seed <= 60; seed++) {
  const rand = rng(seed * 7919);
  const now = GOLDEN_NOW;
  const accounts = Array.from({ length: 1 + Math.floor(rand() * 5) }, (_, i) => makeAccount(rand, now, i));
  // A row listed twice (both dashboard views): the projection keeps the newest reading.
  if (chance(rand, 0.3) && accounts.length > 0) {
    const twin = { ...accounts[0], sampledAt: iso(now - 300 * MINUTE) };
    accounts.push(twin);
  }
  const history = chance(rand, 0.8) ? makeHistory(rand, accounts, now) : null;
  const longBurn = makeLongBurn(rand, accounts);
  const event: AccountAnalyticsSampleEvent = {
    accounts,
    history,
    sampledAt: now,
    ...(longBurn ? { longBurn: new Map(longBurn as Array<[string, never]>) } : {}),
  };
  // Prior one-minute samples feed the recent ring before the published sample.
  const recentSamples: Array<{ accounts: DashboardAccount[]; sampledAt: number }> = [];
  const ring = new RecentReadings();
  const priorCount = Math.floor(rand() * 5);
  for (let p = priorCount; p >= 1; p--) {
    const at = now - p * (1 + Math.floor(rand() * 4)) * MINUTE;
    const priorAccounts = accounts.map((account) => ({
      ...account,
      sampledAt: iso(at),
      windows: account.windows.map((window) => ({
        ...window,
        usedPercent: typeof window.usedPercent === 'number' ? Math.max(0, window.usedPercent - rand() * 6) : window.usedPercent,
        status: undefined,
        sampledAt: undefined,
      })),
    }));
    ring.add(priorAccounts, at);
    recentSamples.push({ accounts: priorAccounts, sampledAt: at });
  }
  const signIn = makeSignIn(rand, now);
  const signInRenewal = chance(rand, 0.4)
    ? {
        runner: 'mac',
        generatedAt: iso(now - Math.floor(rand() * 3 * 60) * MINUTE),
        stale: chance(rand, 0.3),
        needsYou: chance(rand, 0.6)
          ? [{ email: 'someone@example.com', reason: 'captcha', how: chance(rand, 0.5) ? 'Solve the CAPTCHA.' : '', since: chance(rand, 0.5) ? iso(now - 60 * MINUTE) : null }]
          : [],
      }
    : null;
  const refreshSeconds = pick(rand, [30, 60, 60, 120, 300]);
  const signInNow = now + Math.floor(rand() * 10_000);
  const { snapshot, history: historyFile } = buildSnapshotFiles(event, refreshSeconds, ring, signIn as never, signInRenewal as never);
  cases.push({
    seed,
    refreshSeconds,
    signInNow,
    recentSamples,
    event: serializeEvent(event),
    signIn,
    signInRenewal,
    expected: {
      json: `${JSON.stringify(snapshot, null, 2)}\n`,
      markdown: renderSnapshotMarkdown(snapshot, signInNow),
      history: `${JSON.stringify(historyFile)}\n`,
    },
  });
}
fs.writeFileSync(path.join(OUT, 'corpus.json'), JSON.stringify(cases));
console.log(`wrote golden-input.json and corpus.json (${cases.length} cases) to ${OUT}`);
