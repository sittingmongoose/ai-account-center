import { describe, expect, it } from 'bun:test';
import { poolWindow } from '../../../src/antigravity/usage-normalization';
import {
  analyticsWindow,
  appendAccountAnalyticsSnapshot,
} from '../../../src/web-server/services/account-analytics-history';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import { parseUsageReading } from '../../../src/web-server/services/usage-failover-contract';
import { storeWindow } from '../../../src/store/usage-observations';

// RECHECK M3 (2026-11-20): an Antigravity quota service that failed this read (T3's CLI or the app)
// arrives as its known pools with no numbers and status "unavailable". Every layer keeps it (so
// Home keeps the column and Details can name the service), none ever shows it as zero, and it is
// never recorded as an observation.
const UNAVAILABLE = {
  key: 'cli-gemini-argon-wtu-weekly',
  label: 'Gemini 4 Argon · Weekly · In T3 (Antigravity CLI)',
  usedPercent: null,
  remainingPercent: null,
  resetAt: null,
  windowMinutes: 10080,
  used: null,
  limit: null,
  unit: null,
  kind: 'rate_limit',
  status: 'unavailable',
};

describe('a failed Antigravity quota service carried as unavailable pools', () => {
  it('passes the shared window validator only without numbers', () => {
    expect(analyticsWindow(UNAVAILABLE)).toMatchObject({
      key: UNAVAILABLE.key,
      status: 'unavailable',
      usedPercent: null,
      remainingPercent: null,
    });
    expect(analyticsWindow({ ...UNAVAILABLE, usedPercent: 0 })).toBeNull();
    expect(analyticsWindow({ ...UNAVAILABLE, status: 'other' })?.status).toBeUndefined();
  });

  it('passes the Antigravity saved-login normalizer only without numbers', () => {
    expect(poolWindow(UNAVAILABLE)).toMatchObject({
      key: UNAVAILABLE.key,
      status: 'unavailable',
      usedPercent: null,
    });
    expect(poolWindow({ ...UNAVAILABLE, remainingPercent: 100 })).toBeNull();
    expect(poolWindow({ ...UNAVAILABLE, status: undefined })).toBeNull(); // no numbers and no marker: no window
  });

  it('is never stored as an observation', () => {
    const window = analyticsWindow(UNAVAILABLE);
    expect(window).not.toBeNull();
    expect(storeWindow(window!)).toBeNull();
  });

  it('never enters the analytics history', () => {
    const now = Date.parse('2026-11-20T00:00:00Z');
    const live = {
      ...UNAVAILABLE,
      key: 'gemini-weekly',
      label: 'Gemini Models · Weekly · In the Antigravity app',
      usedPercent: 0.09,
      remainingPercent: 99.91,
      resetAt: '2026-11-20T05:47:39.000Z',
      status: undefined,
    };
    const account = {
      id: 'antigravity:profile:gmail',
      provider: 'antigravity',
      providerLabel: 'Antigravity',
      label: 'Fixture',
      email: 'fixture@example.test',
      plan: 'Google AI Pro',
      platform: 'ubuntu',
      source: 'Antigravity saved login on Ubuntu',
      status: 'ok',
      message: null,
      fetchedAt: '2026-11-20T00:00:00.000Z',
      sampledAt: '2026-11-20T00:00:00.000Z',
      isActive: false,
      windows: [UNAVAILABLE, live],
      capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    } as unknown as DashboardAccount;
    const history = appendAccountAnalyticsSnapshot(null, [account], now);
    expect(history.records.at(-1)?.windows.map((w) => w.key)).toEqual(['gemini-weekly']);
  });

  it('survives the failover reading contract next to the live windows', () => {
    const now = Date.parse('2026-11-20T00:00:00Z');
    const reading = parseUsageReading(
      {
        status: 'ok',
        email: 'fixture@example.test',
        plan: 'Google AI Pro',
        sampledAt: '2026-11-20T00:00:00.000Z',
        fetchedAt: '2026-11-20T00:00:00.000Z',
        windows: [
          UNAVAILABLE,
          {
            key: 'gemini-argon-wtu-weekly',
            label: 'Gemini 4 Argon · Weekly · In the Antigravity app',
            usedPercent: 37.83,
            remainingPercent: 62.17,
            resetAt: '2026-11-20T05:47:49.000Z',
            windowMinutes: 10080,
            used: null,
            limit: null,
            unit: null,
            kind: 'rate_limit',
          },
        ],
      },
      now
    );
    expect(reading?.windows.map((w) => [w.key, w.status ?? 'live', w.usedPercent])).toEqual([
      ['cli-gemini-argon-wtu-weekly', 'unavailable', null],
      ['gemini-argon-wtu-weekly', 'live', 37.83],
    ]);
  });
});
