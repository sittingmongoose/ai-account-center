/**
 * Tests for the saved native Codex profile quota collector.
 *
 * The ChatGPT endpoint is never hit: fetches, saved auth and the local session
 * fallback are injected through NativeQuotaDeps (or a stubbed global fetch in
 * the scoped-home tests). A controllable clock drives the TTL, backoff and
 * breaker assertions.
 */

import { beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  getCodexProfileQuotaRows,
  getCachedCodexProfileQuotaRows,
  resetNativeQuotaState,
  type NativeQuotaDeps,
} from '../../../src/web-server/usage/native-quota-collector';
import type { CodexQuotaResult } from '../../../src/cliproxy/quota/quota-types';
import type { CodexLocalQuota } from '../../../src/web-server/usage/codex-local-quota-collector';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';

// A jump comfortably past any single-call backoff cooldown (<= 60s) and the
// 30s parked-row TTL, so the next call is allowed to reach the network.
const MAX_COOLDOWN_JUMP = 61_000;
const TTL_JUMP = 11 * 60 * 1000;

function codexSuccessQuota(): CodexQuotaResult {
  return {
    success: true,
    windows: [],
    coreUsage: {
      fiveHour: {
        label: 'Primary',
        remainingPercent: 60,
        resetAfterSeconds: 3600,
        resetAt: '2026-06-09T19:00:00.000Z',
      },
      weekly: {
        label: 'Secondary',
        remainingPercent: 80,
        resetAfterSeconds: 86400 * 4,
        resetAt: '2026-06-13T00:00:00.000Z',
      },
    },
    planType: 'pro',
    lastUpdated: Date.now(),
    accountId: 'codex-user@example.com',
  };
}

function codexRateLimitedQuota(retryAfter?: string): CodexQuotaResult {
  return {
    success: false,
    windows: [],
    planType: null,
    lastUpdated: Date.now(),
    accountId: 'codex-user@example.com',
    httpStatus: 429,
    retryable: true,
    ...(retryAfter ? { errorDetail: `retry-after:${retryAfter}` } : {}),
    error: 'rate limited',
  };
}

function codexReauthQuota(): CodexQuotaResult {
  return {
    success: false,
    windows: [],
    planType: null,
    lastUpdated: Date.now(),
    accountId: 'codex-user@example.com',
    needsReauth: true,
    error: 'Token expired',
  };
}

function codexLocalQuota(): CodexLocalQuota {
  return {
    quotaPercentage: 30,
    nextReset: '2026-06-09T19:00:00.000Z',
    tier: 'pro',
    stale: true,
    staleAsOf: '2026-06-09T13:30:00.000Z',
    windows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: 70,
        remainingPercent: 30,
        resetAt: '2026-06-09T19:00:00.000Z',
        windowMinutes: 300,
      },
    ],
  };
}

type FixtureAuth = { accessToken: string; accountId: string } | null;

/**
 * Build fully injected deps: saved auth, network fetch, local session fallback
 * and the default profile never touch the real filesystem or network.
 */
function makeDeps(opts: {
  clock: { now: number };
  auth?: (profile: string) => FixtureAuth;
  fetch?: (accessToken: string, accountId: string) => Promise<CodexQuotaResult>;
  local?: () => Promise<CodexLocalQuota | null>;
  defaultProfile?: string | null;
}): NativeQuotaDeps & {
  networkCount: () => number;
  localCount: () => number;
  fetchedAccounts: () => string[];
} {
  let networkCalls = 0;
  let localCalls = 0;
  const fetched: string[] = [];
  const {
    clock,
    auth = (profile: string) => ({ accessToken: `tok-${profile}`, accountId: `id-${profile}` }),
    fetch = async () => codexSuccessQuota(),
    local = async () => codexLocalQuota(),
    defaultProfile = 'default',
  } = opts;
  return {
    readCodexNativeAuth: auth,
    fetchCodexQuotaWithToken: async (accessToken: string, accountId: string) => {
      networkCalls += 1;
      fetched.push(accountId);
      return fetch(accessToken, accountId);
    },
    getCodexQuota: async () => {
      localCalls += 1;
      return local();
    },
    defaultCodexProfile: () => defaultProfile,
    now: () => clock.now,
    sleep: async () => {},
    networkCount: () => networkCalls,
    localCount: () => localCalls,
    fetchedAccounts: () => fetched,
  };
}

async function rowFor(profile: string, deps: NativeQuotaDeps, force = false) {
  const rows = await getCodexProfileQuotaRows([profile], deps, { force });
  return rows.find((row) => row.profile === profile);
}

beforeEach(() => {
  resetNativeQuotaState();
});

describe('Codex network row mapping', () => {
  it('network success builds a fresh row from coreUsage: no staleAsOf, health ok, correct windows', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock });

    const row = await rowFor('default', deps);

    expect(row).toBeDefined();
    expect(row?.provider).toBe('codex');
    expect(row?.surface).toBe('ccsx');
    expect(row?.account_id).toBe('ccsx:default');
    expect(row?.displayName).toBe('default');
    expect(row?.is_subscription).toBe(true);
    expect(row?.is_default).toBe(true);
    expect(row?.paused).toBe(false);
    expect(row?.cached).toBe(false);
    expect(row?.quotaStatus).toBe('ok');
    expect(row?.quotaSource).toBe('network');
    expect(row?.health).toBe('ok');
    expect(row?.tier).toBe('pro');
    expect(row?.needsReauth).toBe(false);
    expect(row?.staleAsOf).toBeUndefined();
    // quota_percentage = min(60, 80) = 60
    expect(row?.quota_percentage).toBe(60);
    // next_reset = soonest = fiveHour resetAt
    expect(row?.next_reset).toBe('2026-06-09T19:00:00.000Z');
    expect(row?.quotaWindows).toHaveLength(2);
    const fiveHr = row?.quotaWindows?.find((w) => w.key === 'five_hour');
    expect(fiveHr?.label).toBe('5h');
    expect(fiveHr?.remainingPercent).toBe(60);
    expect(fiveHr?.usedPercent).toBe(40);
    expect(fiveHr?.windowMinutes).toBe(300);
    expect(fiveHr?.resetAt).toBe('2026-06-09T19:00:00.000Z');
    const week = row?.quotaWindows?.find((w) => w.key === 'seven_day');
    expect(week?.label).toBe('week');
    expect(week?.remainingPercent).toBe(80);
    expect(week?.usedPercent).toBe(20);
    expect(week?.windowMinutes).toBe(10080);
    expect(deps.fetchedAccounts()).toEqual(['id-default']);
    expect(deps.networkCount()).toBe(1);
    expect(deps.localCount()).toBe(0);
  });

  it('a transient network failure on the bare default falls back to the local session row', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      fetch: async () => ({
        success: false,
        windows: [],
        planType: null,
        lastUpdated: Date.now(),
        error: 'network error',
        retryable: true,
      }),
    });

    const row = await rowFor('default', deps);

    expect(row).toBeDefined();
    expect(row?.quotaSource).toBe('local');
    expect(row?.health).toBe('warning');
    expect(row?.staleAsOf).toBe('2026-06-09T13:30:00.000Z');
    expect(deps.localCount()).toBe(1);
  });

  it('needsReauth returns a dimmed reauth row and skips the local fallback', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, fetch: async () => codexReauthQuota() });

    const row = await rowFor('default', deps);

    expect(row?.quotaStatus).toBe('error');
    expect(row?.health).toBe('error');
    expect(row?.needsReauth).toBe(true);
    expect(row?.paused).toBe(true);
    expect(deps.localCount()).toBe(0);
  });

  it('the bare default without saved auth skips the network and reads the local session logs', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, auth: () => null });

    const row = await rowFor('default', deps);

    expect(deps.networkCount()).toBe(0);
    expect(deps.localCount()).toBe(1);
    expect(row?.quotaSource).toBe('local');
    expect(row?.health).toBe('warning');
  });

  it('the bare default with an EMPTY coreUsage falls back to local (no contentless ok row)', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      // Healthy response but no resolved core windows: no glanceable 5h/weekly signal.
      fetch: async () => ({
        success: true,
        windows: [],
        coreUsage: { fiveHour: null, weekly: null },
        planType: 'pro',
        lastUpdated: clock.now,
        accountId: 'codex-user@example.com',
      }),
    });

    const row = await rowFor('default', deps);

    expect(deps.networkCount()).toBe(1);
    expect(deps.localCount()).toBe(1);
    expect(row?.health).toBe('warning');
    expect(row?.staleAsOf).toBe('2026-06-09T13:30:00.000Z');
    expect(row?.quotaWindows).toHaveLength(1);
  });

  it('a named profile with valid auth but a sparse payload yields an active quota-less row', async () => {
    const clock = { now: 7_000_000 };
    const deps = makeDeps({ clock, fetch: async () => ({ success: true }) as CodexQuotaResult });

    const row = await rowFor('ck', deps);

    expect(row).toBeDefined();
    expect(row?.quotaStatus).toBe('ok');
    expect(row?.needsReauth).toBe(false);
    expect(row?.quota_percentage).toBeNull();
    expect(deps.localCount()).toBe(0);
  });

  it('a named profile without saved auth is parked and never filled from local session data', async () => {
    const clock = { now: 5_000_000 };
    const deps = makeDeps({ clock, auth: () => null });

    const row = await rowFor('ck', deps);

    expect(row).toBeDefined();
    expect(row?.paused).toBe(true);
    expect(row?.needsReauth).toBe(true);
    expect(row?.quotaStatus).toBe('unsupported');
    expect(row?.quota_percentage).toBeNull();
    // The global ~/.codex session data is never attributed to a named profile.
    expect(deps.localCount()).toBe(0);
    expect(deps.networkCount()).toBe(0);
  });

  it('the bare default with no saved auth and no local data is parked, never invented', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, auth: () => null, local: async () => null });

    const row = await rowFor('default', deps);

    expect(row?.quotaStatus).toBe('unsupported');
    expect(row?.quota_percentage).toBeNull();
    expect(row?.needsReauth).toBe(true);
    expect(deps.localCount()).toBe(1);
  });
});

describe('Codex local fallback row mapping', () => {
  it('maps a local Codex quota into an ok row', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      auth: () => null,
      local: async () => ({
        quotaPercentage: 52,
        nextReset: '2026-06-09T19:00:00.000Z',
        tier: 'pro',
        stale: false,
        staleAsOf: null,
        windows: [
          {
            key: 'five_hour',
            label: '5h',
            usedPercent: 19,
            remainingPercent: 81,
            resetAt: '2026-06-09T19:00:00.000Z',
            windowMinutes: 300,
          },
          {
            key: 'seven_day',
            label: 'week',
            usedPercent: 48,
            remainingPercent: 52,
            resetAt: '2026-06-14T00:00:00.000Z',
            windowMinutes: 10080,
          },
        ],
      }),
    });

    const row = await rowFor('default', deps);

    expect(row?.quotaStatus).toBe('ok');
    expect(row?.quotaSource).toBe('local');
    expect(row?.quota_percentage).toBe(52);
    expect(row?.tier).toBe('pro');
    expect(row?.health).toBe('ok');
    expect(row?.quotaWindows).toHaveLength(2);
    expect(row?.quotaWindows?.[0].windowMinutes).toBe(300);
    expect(row?.staleAsOf).toBeUndefined();
  });

  it('flags health warning and keeps staleAsOf when the local source is stale', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      auth: () => null,
      local: async () => ({
        quotaPercentage: 10,
        nextReset: null,
        tier: null,
        stale: true,
        staleAsOf: '2026-06-09T13:30:00.000Z',
        windows: [],
      }),
    });

    const row = await rowFor('default', deps);

    expect(row?.health).toBe('warning');
    expect(row?.staleAsOf).toBe('2026-06-09T13:30:00.000Z');
  });
});

describe('cache / TTL', () => {
  it('serves cache within TTL and does NOT re-fetch', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock });

    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(1);

    // Advance < 10 min: still cached.
    clock.now += 5 * 60 * 1000;
    const row = await rowFor('work', deps);
    expect(deps.networkCount()).toBe(1);
    expect(row?.cached).toBe(true);
  });

  it('re-fetches after the TTL expires', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock });

    await rowFor('work', deps);
    clock.now += TTL_JUMP;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(2);
  });

  it('force bypasses the TTL and re-fetches from the network', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock });

    await rowFor('work', deps);
    clock.now += 5 * 60 * 1000;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(1);

    await rowFor('work', deps, true);
    expect(deps.networkCount()).toBe(2);
  });

  it('parked rows use a short TTL so a recovered profile is re-checked quickly', async () => {
    const clock = { now: 7_000_000 };
    let mode: 'fail' | 'ok' = 'fail';
    const deps = makeDeps({
      clock,
      fetch: async () => (mode === 'fail' ? codexRateLimitedQuota() : codexSuccessQuota()),
    });

    // First poll: 429 with no prior cache -> parked row, cached for the short TTL.
    const first = await rowFor('work', deps);
    expect(first?.quotaStatus).toBe('unsupported');
    expect(deps.networkCount()).toBe(1);

    // Past the 30s parked TTL (and the first backoff) but far inside the 10-min
    // quota TTL: the parked row must not be served stale.
    mode = 'ok';
    clock.now += 31_000;
    const second = await rowFor('work', deps);
    expect(second?.quotaStatus).toBe('ok');
    expect(deps.networkCount()).toBe(2);
  });
});

describe('in-flight coalescing', () => {
  it('shares ONE fetch across concurrent callers past TTL', async () => {
    const clock = { now: 1_000_000 };
    let release: (quota: CodexQuotaResult) => void = () => {};
    const gate = new Promise<CodexQuotaResult>((resolve) => {
      release = resolve;
    });
    const deps = makeDeps({ clock, fetch: async () => gate });

    const p1 = rowFor('work', deps);
    const p2 = rowFor('work', deps);
    release(codexSuccessQuota());
    await Promise.all([p1, p2]);

    expect(deps.networkCount()).toBe(1);
  });
});

describe('Retry-After + backoff + circuit breaker', () => {
  it('honors Retry-After: no fetch until the cooldown elapses', async () => {
    const t0 = 1_000_000;
    const clock = { now: t0 };
    const deps = makeDeps({ clock, fetch: async () => codexRateLimitedQuota('50') });

    await rowFor('work', deps); // fetch #1 -> 429, cooldown = t0 + 50s
    expect(deps.networkCount()).toBe(1);

    // Past the 30s parked TTL but inside the 50s Retry-After cooldown -> zero network.
    clock.now = t0 + 40_000;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(1);

    // Past the cooldown -> a fetch is allowed again.
    clock.now = t0 + 51_000;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(2);
  });

  it('trips the breaker after 3 consecutive 429s, then a success closes it', async () => {
    const clock = { now: 1_000_000 };
    let mode: 'fail' | 'ok' = 'fail';
    const deps = makeDeps({
      clock,
      fetch: async () => (mode === 'fail' ? codexRateLimitedQuota() : codexSuccessQuota()),
    });

    // Three 429s, each separated past the parked TTL and the per-call cooldown.
    for (let i = 0; i < 3; i++) {
      await rowFor('work', deps);
      clock.now += MAX_COOLDOWN_JUMP;
    }
    expect(deps.networkCount()).toBe(3);

    // Breaker is open now -> zero network even though the parked row expired.
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(3);

    // Past the 15-min breaker cooldown a success closes it.
    clock.now += 16 * 60 * 1000;
    mode = 'ok';
    const row = await rowFor('work', deps);
    expect(deps.networkCount()).toBe(4);
    expect(row?.quotaStatus).toBe('ok');

    // After the success the breaker is closed: a fetch past TTL proceeds.
    clock.now += TTL_JUMP;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(5);
  });

  it('with the breaker open, the bare default skips the network and serves the local fallback', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, fetch: async () => codexRateLimitedQuota() });

    // Each 429 falls back to the local row, which is cached for the full TTL,
    // so jump past the TTL between the three tripping calls.
    for (let i = 0; i < 3; i++) {
      await rowFor('default', deps);
      clock.now += TTL_JUMP;
    }
    expect(deps.networkCount()).toBe(3);
    const localBefore = deps.localCount();

    // 11 minutes after the third 429 the 15-min breaker is still open.
    const row = await rowFor('default', deps);
    expect(deps.networkCount()).toBe(3);
    expect(deps.localCount()).toBe(localBefore + 1);
    expect(row?.quotaSource).toBe('local');
  });

  it("one profile's 429s do not open another profile's breaker", async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      fetch: async (_token, accountId) =>
        accountId === 'id-work' ? codexRateLimitedQuota() : codexSuccessQuota(),
    });

    for (let i = 0; i < 3; i++) {
      await getCodexProfileQuotaRows(['work', 'ck'], deps);
      clock.now += MAX_COOLDOWN_JUMP;
    }
    // work fetched three times (parked TTL expired each pass); ck once (within TTL).
    expect(deps.fetchedAccounts().filter((id) => id === 'id-work')).toHaveLength(3);
    expect(deps.fetchedAccounts().filter((id) => id === 'id-ck')).toHaveLength(1);

    // Past ck's TTL while work's breaker is still open: only ck reaches the network.
    clock.now += TTL_JUMP;
    const rows = await getCodexProfileQuotaRows(['work', 'ck'], deps);
    expect(deps.fetchedAccounts().filter((id) => id === 'id-work')).toHaveLength(3);
    expect(deps.fetchedAccounts().filter((id) => id === 'id-ck')).toHaveLength(2);
    const ck = rows.find((row) => row.profile === 'ck');
    expect(ck?.quotaStatus).toBe('ok');
    expect(ck?.cached).toBe(false);
  });

  it('a reauth (401) profile is cached and not re-polled within its cooldown, even when forced', async () => {
    const clock = { now: 5_000_000 };
    const deps = makeDeps({ clock, fetch: async () => codexReauthQuota() });

    const first = await rowFor('ck', deps);
    expect(first?.needsReauth).toBe(true);
    expect(first?.paused).toBe(true);
    expect(deps.networkCount()).toBe(1);

    const second = await rowFor('ck', deps, true);
    expect(second?.needsReauth).toBe(true);
    expect(second?.cached).toBe(true);
    expect(deps.networkCount()).toBe(1);
  });

  it('a terminal non-retryable failure (403) cools the profile down instead of re-polling', async () => {
    const t0 = 2_000_000;
    const clock = { now: t0 };
    const deps = makeDeps({
      clock,
      fetch: async () => ({
        success: false,
        windows: [],
        planType: null,
        lastUpdated: clock.now,
        httpStatus: 403,
        retryable: false,
        error: 'forbidden',
      }),
    });

    const first = await rowFor('work', deps);
    expect(first?.quotaStatus).toBe('unsupported');
    expect(deps.networkCount()).toBe(1);

    // Forced, so the parked-row TTL cannot hide the call: only the cooldown
    // (at least the 1 s backoff base) keeps the dead endpoint from being hit.
    clock.now = t0 + 500;
    await rowFor('work', deps, true);
    expect(deps.networkCount()).toBe(1);

    // Past every single-call cooldown the profile is checked once more.
    clock.now = t0 + MAX_COOLDOWN_JUMP;
    await rowFor('work', deps);
    expect(deps.networkCount()).toBe(2);
  });
});

describe('stale-on-fail', () => {
  it('returns the last good row when a later fetch rejects', async () => {
    const clock = { now: 1_000_000 };
    let mode: 'ok' | 'throw' = 'ok';
    const deps = makeDeps({
      clock,
      fetch: async () => {
        if (mode === 'throw') throw new Error('network down');
        return codexSuccessQuota();
      },
    });

    await rowFor('work', deps);
    mode = 'throw';
    clock.now += TTL_JUMP;
    const row = await rowFor('work', deps);
    expect(row?.quotaStatus).toBe('ok');
    expect(row?.cached).toBe(true);
  });

  it('omits the row when the first-ever fetch rejects (no prior cache)', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({
      clock,
      fetch: async () => {
        throw new Error('network down');
      },
    });

    expect(await getCodexProfileQuotaRows(['work'], deps)).toEqual([]);
  });
});

describe('wire fields, default marking and ordering', () => {
  it('rows carry surface "ccsx", account_id "ccsx:<profile>" and the profile as displayName', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, defaultProfile: 'personal' });

    const rows = await getCodexProfileQuotaRows(['personal', 'ck'], deps);

    expect(rows).toHaveLength(2);
    for (const row of rows) {
      expect(row.provider).toBe('codex');
      expect(row.surface).toBe('ccsx');
      expect(row.account_id).toBe(`ccsx:${row.profile}`);
      expect(row.displayName).toBe(row.profile);
      expect(row.is_subscription).toBe(true);
    }
  });

  it('marks only the default profile, sorts rows by profile and collapses duplicates', async () => {
    const clock = { now: 1_000_000 };
    const deps = makeDeps({ clock, defaultProfile: 'personal' });

    const rows = await getCodexProfileQuotaRows(['personal', 'ck', 'personal'], deps);

    expect(rows.map((row) => row.profile)).toEqual(['ck', 'personal']);
    expect(rows.find((row) => row.profile === 'personal')?.is_default).toBe(true);
    expect(rows.find((row) => row.profile === 'ck')?.is_default).toBe(false);
    expect(deps.networkCount()).toBe(2);
  });
});

describe('getCachedCodexProfileQuotaRows (instant, no-fetch fallback)', () => {
  it('returns [] before any collect', () => {
    expect(getCachedCodexProfileQuotaRows(['default', 'ck'])).toEqual([]);
  });

  it('returns the cached rows with is_default kept, without fetching, and [] after reset', async () => {
    const clock = { now: 6_000_000 };
    const deps = makeDeps({ clock, defaultProfile: 'default' });

    await getCodexProfileQuotaRows(['default', 'ck'], deps);
    expect(deps.networkCount()).toBe(2);

    const cached = getCachedCodexProfileQuotaRows(['default', 'ck', 'unknown']);
    expect(cached).toHaveLength(2);
    expect(cached.every((row) => row.cached === true)).toBe(true);
    expect(cached.find((row) => row.profile === 'default')?.is_default).toBe(true);
    expect(cached.find((row) => row.profile === 'ck')?.is_default).toBe(false);
    expect(deps.networkCount()).toBe(2);

    resetNativeQuotaState();
    expect(getCachedCodexProfileQuotaRows(['default', 'ck'])).toEqual([]);
  });
});

describe('quota reset invalidates cached rows', () => {
  function quotaResettingAt(resetIso: string): CodexQuotaResult {
    const base = codexSuccessQuota();
    return {
      ...base,
      coreUsage: {
        fiveHour: {
          label: 'Primary',
          remainingPercent: 60,
          resetAfterSeconds: 60,
          resetAt: resetIso,
        },
        weekly: base.coreUsage!.weekly,
      },
    };
  }

  it('re-fetches once a cached row passes its next_reset, before TTL expiry', async () => {
    const clock = { now: Date.parse('2026-06-09T10:00:00.000Z') };
    const deps = makeDeps({
      clock,
      fetch: async () => quotaResettingAt('2026-06-09T10:01:00.000Z'),
    });

    await rowFor('personal', deps);
    expect(deps.networkCount()).toBe(1);

    // 90s later: past the reset but far inside the 10-min TTL.
    clock.now += 90_000;
    await rowFor('personal', deps);
    expect(deps.networkCount()).toBe(2);
  });

  it('does not refetch-loop when a post-reset payload still reports a past reset', async () => {
    const clock = { now: Date.parse('2026-06-09T10:00:00.000Z') };
    const deps = makeDeps({
      clock,
      fetch: async () => quotaResettingAt('2026-06-09T10:01:00.000Z'),
    });

    await rowFor('personal', deps);
    clock.now += 90_000;
    await rowFor('personal', deps); // refetch fires; payload STILL says 10:01
    expect(deps.networkCount()).toBe(2);

    // The row was fetched after the reset passed, so the stale-by-reset rule
    // must not apply again within the TTL.
    clock.now += 30_000;
    await rowFor('personal', deps);
    expect(deps.networkCount()).toBe(2);
  });
});

describe('default profile resolution from the saved registry', () => {
  function withRegistryHome(
    profilesYaml: string[],
    accounts: Record<string, { paused?: boolean }>,
    run: () => Promise<void>,
    opts: { bareLogin?: boolean } = {}
  ): Promise<void> {
    const originalCcsHome = process.env.CCS_HOME;
    const originalHome = process.env.HOME;
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-default-profile-'));
    process.env.CCS_HOME = tempHome;
    process.env.HOME = tempHome;
    if (opts.bareLogin) {
      // The bare ~/.codex login, under the fixture HOME only.
      const codexDir = path.join(tempHome, '.codex');
      fs.mkdirSync(codexDir, { recursive: true });
      fs.writeFileSync(
        path.join(codexDir, 'auth.json'),
        JSON.stringify({ tokens: { access_token: 'fixture-bare', account_id: 'bare-workspace' } })
      );
    }
    const ccsDir = path.join(tempHome, '.ccs');
    const cliproxyDir = path.join(ccsDir, 'cliproxy');
    fs.mkdirSync(path.join(cliproxyDir, 'auth'), { recursive: true });
    fs.mkdirSync(path.join(cliproxyDir, 'auth-paused'), { recursive: true });
    fs.writeFileSync(path.join(ccsDir, 'codex-profiles.yaml'), profilesYaml.join('\n'));
    const entries: Record<string, Record<string, unknown>> = {};
    for (const [email, account] of Object.entries(accounts)) {
      entries[email] = {
        email,
        tokenFile: `${email}.json`,
        ...(account.paused ? { paused: true } : {}),
      };
      fs.writeFileSync(
        path.join(cliproxyDir, account.paused ? 'auth-paused' : 'auth', `${email}.json`),
        JSON.stringify({ type: 'codex' })
      );
    }
    fs.writeFileSync(
      path.join(cliproxyDir, 'accounts.json'),
      JSON.stringify(
        {
          version: 1,
          providers: { codex: { default: Object.keys(accounts)[0], accounts: entries } },
        },
        null,
        2
      )
    );
    return run().finally(() => {
      if (originalCcsHome !== undefined) process.env.CCS_HOME = originalCcsHome;
      else delete process.env.CCS_HOME;
      if (originalHome !== undefined) process.env.HOME = originalHome;
      else delete process.env.HOME;
      fs.rmSync(tempHome, { recursive: true, force: true });
    });
  }

  function profileYaml(name: string, created: string): string[] {
    return [
      `  ${name}:`,
      '    type: codex',
      `    created: "${created}"`,
      '    last_used: null',
      `    email: ${name}-codex@example.com`,
      `    account_id: ${name}-codex@example.com`,
    ];
  }

  function registryDeps(): NativeQuotaDeps {
    return {
      readCodexNativeAuth: (profile: string) => ({
        accessToken: `token-${profile}`,
        accountId: `${profile}-codex@example.com`,
      }),
      fetchCodexQuotaWithToken: async (_token: string, accountId: string) => ({
        ...codexSuccessQuota(),
        accountId,
      }),
      getCodexQuota: async () => null,
      now: () => 1_000_000,
      sleep: async () => {},
    };
  }

  it('skips a paused registry default and marks the first unpaused saved profile', async () => {
    await withRegistryHome(
      [
        'version: "1.0"',
        'default: paused',
        'profiles:',
        ...profileYaml('paused', '2026-01-01T00:00:00.000Z'),
        ...profileYaml('active', '2026-01-02T00:00:00.000Z'),
        '',
      ],
      { 'paused-codex@example.com': { paused: true }, 'active-codex@example.com': {} },
      async () => {
        const rows = await getCodexProfileQuotaRows(['paused', 'active'], registryDeps());
        expect(rows.find((row) => row.profile === 'active')?.is_default).toBe(true);
        expect(rows.find((row) => row.profile === 'paused')?.is_default).toBe(false);
      }
    );
  });

  const allPausedRegistry = [
    'version: "1.0"',
    'default: paused',
    'profiles:',
    ...profileYaml('paused', '2026-01-01T00:00:00.000Z'),
    '',
  ];

  it('resolves no default when every saved profile is paused and no bare login exists', async () => {
    await withRegistryHome(
      allPausedRegistry,
      { 'paused-codex@example.com': { paused: true } },
      async () => {
        expect(fs.existsSync(path.join(os.homedir(), '.codex', 'auth.json'))).toBe(false);
        const rows = await getCodexProfileQuotaRows(['paused', 'default'], registryDeps());
        expect(rows.map((row) => row.profile)).toEqual(['default', 'paused']);
        expect(rows.find((row) => row.profile === 'paused')?.is_default).toBe(false);
        // Without a bare login, the bare 'default' row is not the default either.
        expect(rows.find((row) => row.profile === 'default')?.is_default).toBe(false);
      }
    );
  });

  it('falls back to the bare login as default when every saved profile is paused', async () => {
    await withRegistryHome(
      allPausedRegistry,
      { 'paused-codex@example.com': { paused: true } },
      async () => {
        const rows = await getCodexProfileQuotaRows(['paused', 'default'], registryDeps());
        expect(rows.map((row) => row.profile)).toEqual(['default', 'paused']);
        expect(rows.find((row) => row.profile === 'default')?.is_default).toBe(true);
        expect(rows.find((row) => row.profile === 'paused')?.is_default).toBe(false);
      },
      { bareLogin: true }
    );
  });
});

describe('saved native Codex dashboard quota', () => {
  function saveAuth(home: string, token: string, workspace: string): string {
    const dir = path.join(home, '.ccs', 'codex-instances', 'gmail');
    fs.mkdirSync(dir, { recursive: true });
    const authPath = path.join(dir, 'auth.json');
    fs.writeFileSync(
      authPath,
      JSON.stringify({ tokens: { access_token: token, account_id: workspace } })
    );
    return authPath;
  }

  it('uses each nested native token and workspace without CLIProxy credentials', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-native-codex-quota-'));
    const originalFetch = global.fetch;
    const requests: Array<{ token: string | null; workspace: string | null }> = [];
    const profiles = ['gmail', 'party', 'lime'];
    try {
      for (const profile of profiles) {
        const dir = path.join(tempHome, '.ccs', 'codex-instances', profile);
        fs.mkdirSync(dir, { recursive: true });
        fs.writeFileSync(
          path.join(dir, 'auth.json'),
          JSON.stringify({
            tokens: { access_token: `fake-native-${profile}`, account_id: `workspace-${profile}` },
          })
        );
      }
      global.fetch = (async (_input, init) => {
        const headers = new Headers(init?.headers);
        requests.push({
          token: headers.get('Authorization'),
          workspace: headers.get('ChatGPT-Account-Id'),
        });
        return new Response(
          JSON.stringify({
            plan_type: 'pro',
            rate_limit: {
              primary_window: { used_percent: 25, reset_after_seconds: 3600 },
              secondary_window: { used_percent: 55, reset_after_seconds: 86400 },
            },
          })
        );
      }) as typeof fetch;
      await runWithScopedCcsHome(tempHome, async () => {
        const rows = await getCodexProfileQuotaRows(profiles, {
          defaultCodexProfile: () => 'gmail',
        });
        expect(rows.map((row) => row.profile)).toEqual(['gmail', 'lime', 'party']);
        expect(rows.every((row) => row.quotaStatus === 'ok')).toBe(true);
        expect(rows.every((row) => row.quotaWindows?.length === 2)).toBe(true);
        expect(rows[0].quotaWindows?.map((window) => window.usedPercent)).toEqual([25, 55]);
        expect(requests).toHaveLength(3);
        for (const profile of profiles) {
          expect(requests).toContainEqual({
            token: `Bearer fake-native-${profile}`,
            workspace: `workspace-${profile}`,
          });
        }
        expect(fs.existsSync(path.join(tempHome, '.ccs', 'cliproxy', 'auth'))).toBe(false);
        await getCodexProfileQuotaRows(profiles, { defaultCodexProfile: () => 'gmail' });
        expect(requests).toHaveLength(3);
      });
    } finally {
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('fills the third profile in a bounded second batch without waiting for another poll', async () => {
    let active = 0;
    let maxActive = 0;
    const called: string[] = [];
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const pending = getCodexProfileQuotaRows(['gmail', 'party', 'lime'], {
      defaultCodexProfile: () => 'gmail',
      readCodexNativeAuth: (profile) => ({ accessToken: `fake-${profile}`, accountId: profile }),
      fetchCodexQuotaWithToken: async (_token, accountId) => {
        called.push(accountId);
        active += 1;
        maxActive = Math.max(maxActive, active);
        await gate;
        active -= 1;
        return codexSuccessQuota();
      },
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(called).toHaveLength(2);
    release();
    const rows = await pending;
    expect(maxActive).toBe(2);
    expect(called).toHaveLength(3);
    expect(rows).toHaveLength(3);
    expect(getCachedCodexProfileQuotaRows(['gmail', 'party', 'lime'])).toHaveLength(3);
  });

  it('allows a requested profile refresh to bypass TTL while coalescing the forced fetch', async () => {
    let calls = 0;
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const deps: NativeQuotaDeps = {
      defaultCodexProfile: () => 'gmail',
      readCodexNativeAuth: () => ({ accessToken: 'fake-token', accountId: 'fixture-workspace' }),
      fetchCodexQuotaWithToken: async () => {
        calls += 1;
        if (calls > 1) await gate;
        return codexSuccessQuota();
      },
    };
    await getCodexProfileQuotaRows(['gmail'], deps);
    await getCodexProfileQuotaRows(['gmail'], deps);
    expect(calls).toBe(1);
    const first = getCodexProfileQuotaRows(['gmail'], deps, { force: true });
    const second = getCodexProfileQuotaRows(['gmail'], deps, { force: true });
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(2);
    release();
    expect(await first).toHaveLength(1);
    expect(await second).toHaveLength(1);
    expect(calls).toBe(2);
  });

  it('isolates same-named profile quotas and cache projections across CCS scopes', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-scoped-quota-'));
    const originalFetch = global.fetch;
    let calls = 0;
    try {
      const firstHome = path.join(tempHome, 'one');
      const secondHome = path.join(tempHome, 'two');
      saveAuth(firstHome, 'fake-one', 'workspace-one');
      saveAuth(secondHome, 'fake-two', 'workspace-two');
      global.fetch = (async (_input, init) => {
        calls += 1;
        const used =
          new Headers(init?.headers).get('ChatGPT-Account-Id') === 'workspace-one' ? 25 : 75;
        return new Response(
          JSON.stringify({
            rate_limit: { primary_window: { used_percent: used, reset_after_seconds: 3600 } },
          })
        );
      }) as typeof fetch;
      for (const [home, used] of [
        [firstHome, 25],
        [secondHome, 75],
      ] as const) {
        await runWithScopedCcsHome(home, async () => {
          const rows = await getCodexProfileQuotaRows(['gmail'], {
            defaultCodexProfile: () => 'gmail',
          });
          expect(rows[0].quotaWindows?.[0].usedPercent).toBe(used);
          expect(getCachedCodexProfileQuotaRows(['gmail'])[0].quotaWindows?.[0].usedPercent).toBe(
            used
          );
          expect(getCachedCodexProfileQuotaRows(['gmail'])).toHaveLength(1);
        });
      }
      expect(calls).toBe(2);
    } finally {
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('drops replaced or removed auth from cache before TTL and fetches the new identity', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-auth-cache-'));
    const originalFetch = global.fetch;
    let calls = 0;
    try {
      const authPath = saveAuth(tempHome, 'fake-old', 'old-workspace');
      global.fetch = (async (_input, init) => {
        calls += 1;
        const used =
          new Headers(init?.headers).get('ChatGPT-Account-Id') === 'old-workspace' ? 25 : 75;
        return new Response(
          JSON.stringify({
            rate_limit: { primary_window: { used_percent: used, reset_after_seconds: 3600 } },
          })
        );
      }) as typeof fetch;
      await runWithScopedCcsHome(tempHome, async () => {
        await getCodexProfileQuotaRows(['gmail'], { defaultCodexProfile: () => 'gmail' });
        saveAuth(tempHome, 'fake-new', 'new-workspace');
        expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
        const rows = await getCodexProfileQuotaRows(['gmail'], {
          defaultCodexProfile: () => 'gmail',
        });
        expect(rows[0].quotaWindows?.[0].usedPercent).toBe(75);
        expect(calls).toBe(2);
        fs.unlinkSync(authPath);
        expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
        const absent = await getCodexProfileQuotaRows(['gmail'], {
          defaultCodexProfile: () => 'gmail',
        });
        expect(absent[0].quotaWindows).toBeUndefined();
        expect(absent[0].needsReauth).toBe(true);
        expect(calls).toBe(2);
      });
    } finally {
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('discards the cache when auth.json is rewritten with the same access token', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-auth-rewrite-'));
    const originalFetch = global.fetch;
    const workspaces: Array<string | null> = [];
    try {
      const authPath = saveAuth(tempHome, 'fake-same', 'old-workspace');
      global.fetch = (async (_input, init) => {
        const workspace = new Headers(init?.headers).get('ChatGPT-Account-Id');
        workspaces.push(workspace);
        return new Response(
          JSON.stringify({
            rate_limit: {
              primary_window: {
                used_percent: workspace === 'old-workspace' ? 25 : 75,
                reset_after_seconds: 3600,
              },
            },
          })
        );
      }) as typeof fetch;
      const deps: NativeQuotaDeps = { defaultCodexProfile: () => 'gmail' };
      await runWithScopedCcsHome(tempHome, async () => {
        await getCodexProfileQuotaRows(['gmail'], deps);
        expect(workspaces).toEqual(['old-workspace']);

        // Same access token, different workspace: the new workspace is fetched.
        saveAuth(tempHome, 'fake-same', 'new-workspace');
        expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
        const moved = await getCodexProfileQuotaRows(['gmail'], deps);
        expect(moved[0].quotaWindows?.[0].usedPercent).toBe(75);
        expect(workspaces).toEqual(['old-workspace', 'new-workspace']);

        // Same access token and workspace, but the saved file changed (here the
        // refresh token): the file digest, not the token, decides cache reuse.
        fs.writeFileSync(
          authPath,
          JSON.stringify({
            tokens: {
              access_token: 'fake-same',
              account_id: 'new-workspace',
              refresh_token: 'fixture-refresh',
            },
          })
        );
        expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
        await getCodexProfileQuotaRows(['gmail'], deps);
        expect(workspaces).toEqual(['old-workspace', 'new-workspace', 'new-workspace']);
      });
    } finally {
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('does not carry an expired account cooldown into a replacement saved login', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-auth-cooldown-'));
    const originalFetch = global.fetch;
    let calls = 0;
    try {
      saveAuth(tempHome, 'fake-expired', 'old-workspace');
      global.fetch = (async (_input, init) => {
        calls += 1;
        return new Headers(init?.headers).get('ChatGPT-Account-Id') === 'old-workspace'
          ? new Response('{}', { status: 401 })
          : new Response(
              JSON.stringify({
                rate_limit: { primary_window: { used_percent: 75, reset_after_seconds: 3600 } },
              })
            );
      }) as typeof fetch;
      await runWithScopedCcsHome(tempHome, async () => {
        const expired = await getCodexProfileQuotaRows(['gmail'], {
          defaultCodexProfile: () => 'gmail',
        });
        expect(expired[0].needsReauth).toBe(true);
        saveAuth(tempHome, 'fake-current', 'new-workspace');
        const replacement = await getCodexProfileQuotaRows(['gmail'], {
          defaultCodexProfile: () => 'gmail',
        });
        expect(replacement[0].quotaWindows?.[0].usedPercent).toBe(75);
        expect(replacement[0].needsReauth).toBe(false);
        expect(calls).toBe(2);
      });
    } finally {
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('discards an old login result that completes after saved auth was replaced', async () => {
    const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-inflight-auth-'));
    const originalFetch = global.fetch;
    let release: (response: Response) => void = () => {};
    const oldResponse = new Promise<Response>((resolve) => {
      release = resolve;
    });
    let calls = 0;
    const response = (used: number) =>
      new Response(
        JSON.stringify({
          rate_limit: { primary_window: { used_percent: used, reset_after_seconds: 3600 } },
        })
      );
    try {
      saveAuth(tempHome, 'fake-old', 'old-workspace');
      global.fetch = (async (_input, init) => {
        calls += 1;
        return new Headers(init?.headers).get('ChatGPT-Account-Id') === 'old-workspace'
          ? oldResponse
          : response(75);
      }) as typeof fetch;
      await runWithScopedCcsHome(tempHome, async () => {
        const oldPending = getCodexProfileQuotaRows(['gmail'], {
          defaultCodexProfile: () => 'gmail',
        });
        await Promise.resolve();
        await Promise.resolve();
        expect(calls).toBe(1);
        saveAuth(tempHome, 'fake-new', 'new-workspace');
        expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
        await getCodexProfileQuotaRows(['gmail'], { defaultCodexProfile: () => 'gmail' });
        release(response(25));
        const completed = await oldPending;
        expect(completed[0].quotaWindows?.[0].usedPercent).toBe(75);
        expect(getCachedCodexProfileQuotaRows(['gmail'])[0].quotaWindows?.[0].usedPercent).toBe(75);
        expect(calls).toBe(2);
      });
    } finally {
      release(response(25));
      global.fetch = originalFetch;
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  it('cannot return old-account stale usage when its pending refresh rejects after replacement', async () => {
    let auth = { accessToken: 'fake-old', accountId: 'old-workspace' };
    const clock = { now: 1_000_000 };
    let calls = 0;
    let reject: (error: Error) => void = () => {};
    const failingRefresh = new Promise<CodexQuotaResult>((_resolve, rejectPromise) => {
      reject = rejectPromise;
    });
    const deps: NativeQuotaDeps = {
      defaultCodexProfile: () => 'gmail',
      readCodexNativeAuth: () => auth,
      now: () => clock.now,
      fetchCodexQuotaWithToken: async () => {
        calls += 1;
        if (calls > 1) return failingRefresh;
        const quota = codexSuccessQuota();
        quota.coreUsage!.fiveHour!.remainingPercent = 75;
        return quota;
      },
    };
    await getCodexProfileQuotaRows(['gmail'], deps);
    clock.now += 601_000;
    const pending = getCodexProfileQuotaRows(['gmail'], deps);
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(2);
    auth = { accessToken: 'fake-new', accountId: 'new-workspace' };
    reject(new Error('Synthetic request failed'));
    expect(await pending).toEqual([]);
    expect(getCachedCodexProfileQuotaRows(['gmail'])).toEqual([]);
  });
});
